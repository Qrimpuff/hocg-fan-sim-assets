use std::{collections::BTreeMap, fs::File, io::BufReader, path::Path, sync::Arc};

use hocg_fan_sim_assets_model::CardsDatabase;
use itertools::Itertools;
use parking_lot::Mutex;
use rayon::iter::{IntoParallelIterator, IntoParallelRefMutIterator, ParallelIterator};

use crate::{
    DEBUG,
    images::utils::{dist_hash, to_image_hash},
};

// one image can map to multiple illustrations, if they are similar enough
pub const DIST_TOLERANCE: u64 = 2 * 2 * 2 * 2; // a distance of 2 in each channel

pub fn import_holodelta(
    all_cards: &mut CardsDatabase,
    images_jp_path: &Path,
    holodelta_path: &Path,
) {
    println!("Importing holoDelta images...");

    let conn = rusqlite::Connection::open(holodelta_path).unwrap();

    let mut stmt = conn
        .prepare("SELECT cardID, art_index, lang, art FROM 'cardHasArt' WHERE lang = 'ja' ORDER BY cardID, art_index")
        .unwrap();
    let delta_cards = stmt
        .query_map([], |row| {
            Ok((
                row.get::<usize, String>(0).unwrap(),
                row.get::<usize, u32>(1).unwrap(),
                row.get::<usize, String>(2).unwrap(),
                row.get::<usize, Vec<u8>>(3).unwrap(),
            ))
        })
        .unwrap();

    // for every card number, find the best match
    let mut total_count = 0;
    let mut updated_count = 0;
    for delta_cards in delta_cards
        .filter_map(|c| c.ok())
        .into_group_map_by(|c| c.0.clone())
    {
        total_count += delta_cards.1.len();
        let card_number = delta_cards.0;

        if DEBUG {
            println!("\nProcessing card {:?}", card_number);
        }

        // update holoDelta art indexes, based on card image
        if let Some(card) = all_cards.get_mut(&card_number) {
            let delta_cards: Vec<_> = delta_cards
                .1
                .into_par_iter()
                .map(|delta_card| {
                    let delta_img = image::load_from_memory(&delta_card.3).unwrap();
                    let delta_img = delta_img.into_rgb8();
                    (delta_card, delta_img)
                })
                .collect();

            let cards: Vec<_> = card
                .illustrations
                .par_iter_mut()
                .map(|illust| {
                    let path = images_jp_path.join(&illust.img_path.japanese);
                    let f = File::open(&path).unwrap();
                    let f = BufReader::new(f);
                    let card_img = image::load(f, image::ImageFormat::WebP).unwrap();
                    let card_img = card_img.into_rgb8();

                    // clear the delta art index, will be set later
                    illust.delta_art_index = None;

                    (Arc::new(Mutex::new(illust)), card_img)
                })
                .collect();

            let mut dists = delta_cards
                .iter()
                .cartesian_product(cards.iter())
                .map(|((delta_card, delta_img), (card, card_img))| {
                    let h1 = to_image_hash(delta_img);
                    let h2 = to_image_hash(card_img);

                    let dist = dist_hash(&h1, &h2);

                    if DEBUG {
                        let card = card.lock();
                        println!("holoDelta hash: {} = {}", delta_card.1, h1);
                        println!(
                            "Card hash: {} {} = {}",
                            card.card_number,
                            card.manage_id.unwrap(),
                            h2
                        );
                        println!("Distance: {}", dist);
                    }

                    (delta_card.1, card, dist)
                })
                .collect_vec();

            // sort by best dist, then update the art index
            dists.sort_by_key(|d| d.2);

            // modify the cards here, to avoid borrowing issue
            let mut already_set = BTreeMap::new();
            for (delta_art_index, card, dist) in dists {
                // println!("dist: {:?}", (delta_art_index, card, dist));

                let mut card = card.lock();
                // to handle multiple cards with the same image
                let min_dist = *already_set
                    .get(&delta_art_index)
                    .unwrap_or(&(u64::MAX - DIST_TOLERANCE));
                if card.delta_art_index.is_none() && min_dist + DIST_TOLERANCE >= dist {
                    card.delta_art_index = Some(delta_art_index);
                    already_set.insert(delta_art_index, dist.min(min_dist));
                    updated_count += 1;

                    if DEBUG {
                        println!(
                            "Updated card {:?} -> manage_id: {}, delta_art_index: {} ({})",
                            card.card_number,
                            card.manage_id.unwrap(),
                            card.delta_art_index.unwrap(),
                            dist
                        );
                    }
                }
            }
        }
    }

    println!("Processed {} holoDelta cards", total_count);
    println!("Updated {} hOCG cards", updated_count);
}
