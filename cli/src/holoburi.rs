use std::collections::BTreeMap;

use hocg_fan_sim_assets_model::{
    CardRarity, CardsDatabase,
    img_hash::{dist_hash, to_image_hash},
};
use image::{ImageFormat, RgbImage};
use itertools::Itertools;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};

use crate::{http_client, utils::SharedImageMatches};

const HOLOBURI_RARITIES_URL: &str = "https://holoburi.com/cards/rarities.json";
const HOLOBURI_CARDS_URL: &str = "https://holoburi.com/cards/";

// card number -> art keys, e.g. "hBP02-033" -> ["SR", "SR_2"]
type HoloburiRarities = BTreeMap<String, Vec<String>>;

pub fn import_holoburi(all_cards: &mut CardsDatabase) {
    println!("Importing holoBuri art keys...");

    let Some(rarities) = fetch_rarities() else {
        return;
    };

    let mut total_count = 0;
    let mut updated_count = 0;
    for (card_number, art_keys) in rarities {
        total_count += art_keys.len();

        let Some(card) = all_cards.get_mut(&card_number) else {
            println!("holoBuri card not found in hOCG cards: {card_number}");
            continue;
        };

        // download every art first, so a failed download keeps the existing keys
        let Some(arts) = art_keys
            .par_iter()
            .map(|art_key| download_art(&card_number, art_key).map(|img| (art_key.as_str(), img)))
            .collect::<Option<Vec<_>>>()
        else {
            eprintln!("Skipping {card_number}: failed to download holoBuri art");
            continue;
        };

        let illustrations = &card.illustrations;
        let mut dists: Vec<(&str, usize, u64)> = arts
            .par_iter()
            .flat_map(|(art_key, img)| {
                let h1 = to_image_hash(img, None);
                illustrations
                    .iter()
                    .enumerate()
                    // holoBuri gives the rarity, so only compare illustrations of that rarity.
                    // illustrations without an image hash can't be compared
                    .filter(|(_, illust)| {
                        illust.rarity == art_rarity(art_key) && !illust.img_hash.is_empty()
                    })
                    .map(|(idx, illust)| (*art_key, idx, dist_hash(&h1, &illust.img_hash)))
                    .collect_vec()
            })
            .collect();

        // sort by best dist, then update the art key
        dists.sort_by_key(|(_, idx, dist)| {
            (
                *dist,
                illustrations[*idx]
                    .manage_id
                    .japanese
                    .iter()
                    .flatten()
                    .copied()
                    .next()
                    .unwrap_or(u32::MAX),
            )
        });

        for illust in card.illustrations.iter_mut() {
            illust.holoburi_art_key = None;
        }

        // to handle multiple illustrations with the same image
        let mut matches = SharedImageMatches::default();
        for (art_key, idx, dist) in dists {
            let illust = &mut card.illustrations[idx];
            if illust.holoburi_art_key.is_none() && matches.accept(art_key, illust, dist) {
                illust.holoburi_art_key = Some(art_key.to_string());
                updated_count += 1;
            }
        }

        for art_key in &art_keys {
            if !matches.contains(&art_key.as_str()) {
                println!("NO MATCH: [{card_number}, {art_key}]");
            }
        }
    }

    println!("Processed {total_count} holoBuri art keys");
    println!("Updated {updated_count} hOCG illustrations");
}

fn fetch_rarities() -> Option<HoloburiRarities> {
    let resp = http_client()
        .get(HOLOBURI_RARITIES_URL)
        .send()
        .map_err(|e| eprintln!("Failed to download holoBuri rarities: {e:?}"))
        .ok()?;
    if !resp.status().is_success() {
        eprintln!(
            "Failed to download holoBuri rarities (status: {})",
            resp.status()
        );
        return None;
    }
    let text = resp
        .text()
        .map_err(|e| eprintln!("Failed to read holoBuri rarities: {e:?}"))
        .ok()?;
    serde_json::from_str(&text)
        .map_err(|e| eprintln!("Failed to parse holoBuri rarities: {e:?}"))
        .ok()
}

fn download_art(card_number: &str, art_key: &str) -> Option<RgbImage> {
    let url = format!("{HOLOBURI_CARDS_URL}{card_number}_{art_key}.webp");
    let resp = http_client()
        .get(url.as_str())
        .send()
        .map_err(|e| eprintln!("Failed to download image: {url} {e:?}"))
        .ok()?;
    if !resp.status().is_success() {
        eprintln!(
            "Failed to download image (status: {}): {url}",
            resp.status()
        );
        return None;
    }
    let bytes = resp
        .bytes()
        .map_err(|e| eprintln!("Failed to read image: {url} {e:?}"))
        .ok()?;
    image::load_from_memory_with_format(&bytes, ImageFormat::WebP)
        .map_err(|e| eprintln!("Failed to decode image: {url} {e:?}"))
        .ok()
        .map(|img| img.into_rgb8())
}

// the rarity is the part before the "_N" suffix, e.g. "SR_2" -> SR
fn art_rarity(art_key: &str) -> CardRarity {
    let rarity = art_key.split('_').next().unwrap_or_default();
    CardRarity::from(rarity.to_string())
}
