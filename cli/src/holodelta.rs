use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::BufReader,
    path::Path,
    sync::Arc,
};

use hocg_fan_sim_assets_model::CardsInfo2;
use image::{
    GrayImage,
    imageops::{crop, resize},
};
use imageproc::map::{blue_channel, green_channel, red_channel};
use itertools::Itertools;
use parking_lot::Mutex;
use rayon::iter::{
    IntoParallelIterator, IntoParallelRefMutIterator, ParallelIterator,
};

pub fn import_holodelta(all_cards: &mut CardsInfo2, images_jp_path: &Path, holodelta_path: &Path) {
    const DEBUG: bool = false;

    println!("Importing holoDelta images...");

    const SHRINK_FACTOR: u32 = 20;
    const IMAGE_WIDTH: u32 = 744 / SHRINK_FACTOR;
    const IMAGE_HEIGHT: u32 = 1040 / SHRINK_FACTOR;
    const CORNER: u32 = 20 / SHRINK_FACTOR;
    const IMAGE_WIDTH_CROP: u32 = IMAGE_WIDTH - CORNER - CORNER;
    const IMAGE_HEIGHT_CROP: u32 = IMAGE_HEIGHT - CORNER - CORNER;
    const DIFF_TOLERANCE: f64 = 6.0 / (IMAGE_WIDTH_CROP * IMAGE_HEIGHT_CROP * 3) as f64 * 100.0; // accept 2 pixels (rgb) difference

    fn prepare_image(image: &mut GrayImage) {
        let mut img = resize(
            image,
            IMAGE_WIDTH,
            IMAGE_HEIGHT,
            image::imageops::FilterType::Gaussian,
        );
        *image = crop(
            &mut img,
            CORNER,
            CORNER,
            IMAGE_WIDTH_CROP,
            IMAGE_HEIGHT_CROP,
        )
        .to_image();
    }

    fn compare_images(card_img: &image::GrayImage, delta_img: &image::GrayImage) -> (i64, Vec<u8>) {
        // try shifting the delta image up/down/left/right to find a better match
        let mut full_diff = 0;
        let mut img_diff = vec![];
        for (x, y, c) in card_img.enumerate_pixels() {
            // check adjacent pixels
            let pxs = [
                (0, 0),
                (1, 0),
                (0, 1),
                (-1, 0),
                (0, -1),
                (1, 1),
                (-1, 1),
                (1, -1),
                (-1, -1),
            ];
            let diffs: Vec<_> = pxs
                .into_iter()
                .filter_map(|(dx, dy)| {
                    // check if the pixel is within bounds
                    let d = delta_img
                        .get_pixel_checked(x.wrapping_add_signed(dx), y.wrapping_add_signed(dy))?;
                    let diff = (d.0[0] as i64 - c.0[0] as i64).abs();
                    Some(diff)
                })
                .collect();
            let diff = diffs.into_iter().min().unwrap_or(i64::MAX);

            // penalize big differences
            if diff > 50 {
                full_diff += 2;
                img_diff.push(255);
            } else if diff > 5 {
                full_diff += 1;
                img_diff.push(128);
            } else {
                img_diff.push(0);
            }
        }

        (full_diff, img_diff)
    }

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
            println!("Processing card {:?}", card_number);
        }

        // update holoDelta art indexes, based on card image
        if let Some(cards) = all_cards.get_mut(&card_number) {
            let delta_cards: Vec<_> = delta_cards
                .1
                .into_par_iter()
                .map(|delta_card| {
                    let delta_img = image::load_from_memory(&delta_card.3).unwrap();
                    // prepare the delta image
                    let delta_img = delta_img.to_rgb8();
                    let mut delta_img_r = red_channel(&delta_img);
                    let mut delta_img_g = green_channel(&delta_img);
                    let mut delta_img_b = blue_channel(&delta_img);
                    prepare_image(&mut delta_img_r);
                    prepare_image(&mut delta_img_g);
                    prepare_image(&mut delta_img_b);

                    (delta_card, delta_img_r, delta_img_g, delta_img_b)
                })
                .collect();

            let cards: Vec<_> = cards
                .illustrations
                .par_iter_mut()
                .map(|illust| {
                    let path = images_jp_path.join(&illust.img_path.japanese);
                    let f = File::open(&path).unwrap();
                    let f = BufReader::new(f);
                    let card_img = image::load(f, image::ImageFormat::WebP).unwrap();

                    // prepare the existing official image
                    let card_img = card_img.to_rgb8();
                    let mut card_img_r = red_channel(&card_img);
                    let mut card_img_g = green_channel(&card_img);
                    let mut card_img_b = blue_channel(&card_img);
                    prepare_image(&mut card_img_r);
                    prepare_image(&mut card_img_g);
                    prepare_image(&mut card_img_b);

                    // clear the delta art index, will be set later
                    illust.delta_art_index = None;

                    (
                        Arc::new(Mutex::new(illust)),
                        card_img_r,
                        card_img_g,
                        card_img_b,
                    )
                })
                .collect();

            let mut best_diff = f64::MAX;
            let mut worst_diff = f64::MIN; //0.00000007037019349668126;

            let mut diffs = delta_cards
                .iter()
                .cartesian_product(cards.iter())
                .map(
                    |(
                        (delta_card, delta_img_r, delta_img_g, delta_img_b),
                        (card, card_img_r, card_img_g, card_img_b),
                    )| {
                        let (full_r_diff, img_diff_r) = compare_images(card_img_r, delta_img_r);
                        let (full_g_diff, img_diff_g) = compare_images(card_img_g, delta_img_g);
                        let (full_b_diff, img_diff_b) = compare_images(card_img_b, delta_img_b);

                        let full_diff = full_r_diff + full_g_diff + full_b_diff;
                        let img_diff = img_diff_r
                            .into_iter()
                            .zip(img_diff_g)
                            .zip(img_diff_b)
                            .flat_map(|((r, g), b)| [r, g, b])
                            .collect::<Vec<_>>();

                        let diff = full_diff as f64 / img_diff.len() as f64 * 100.0;
                        if diff < best_diff {
                            best_diff = diff;
                        }
                        if diff > worst_diff {
                            worst_diff = diff;
                        }

                        if DEBUG {
                            let card = card.lock();
                            println!(
                                "Compare {:?} {}_{} ({full_diff}/{} {:2.2}%)",
                                card.card_number,
                                card.manage_id.unwrap(),
                                delta_card.1,
                                img_diff.len(),
                                full_diff as f64 / img_diff.len() as f64 * 100.0,
                            );

                            let delta_img = image::DynamicImage::ImageRgb8(
                                image::RgbImage::from_vec(
                                    IMAGE_WIDTH_CROP,
                                    IMAGE_HEIGHT_CROP,
                                    img_diff,
                                )
                                .unwrap(),
                            );
                            fs::create_dir_all("diff").unwrap();
                            delta_img
                                .save(format!(
                                    "diff/diff_{}_{}_{}.png",
                                    card.card_number,
                                    card.manage_id.unwrap(),
                                    delta_card.1
                                ))
                                .unwrap();
                        }

                        (delta_card.1, card, diff)
                    },
                )
                .collect_vec();

            // sort by best diff, then update the art index
            diffs.sort_by(|d1, d2| d1.2.partial_cmp(&d2.2).unwrap());

            // println!("1 BEST DIFF - {:>05.2}% - {}", best_diff, card_number);
            // println!("2 WORST DIFF - {:>05.2}% - {}", worst_diff, card_number);

            // modify the cards here, to avoid borrowing issue
            let mut already_set = BTreeMap::new();
            for (delta_art_index, card, diff) in diffs {
                // the image is too different
                if diff > 50.0 {
                    continue;
                }

                let mut card = card.lock();
                // to handle multiple cards with the same image
                let min_diff = *already_set.get(&delta_art_index).unwrap_or(&100.0);
                if card.delta_art_index.is_none() && min_diff + DIFF_TOLERANCE >= diff {
                    card.delta_art_index = Some(delta_art_index);
                    already_set.insert(delta_art_index, diff.min(min_diff));
                    updated_count += 1;

                    if DEBUG {
                        println!(
                            "Updated card {:?} -> manage_id: {}, delta_art_index: {} ({:2.2}%)",
                            card.card_number,
                            card.manage_id.unwrap(),
                            card.delta_art_index.unwrap(),
                            diff
                        );
                    }
                }
            }
        }
    }

    println!("Processed {} holoDelta cards", total_count);
    println!("Updated {} hOCG cards", updated_count);
}
