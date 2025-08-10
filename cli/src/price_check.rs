use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Duration,
};

use hocg_fan_sim_assets_model::{CardIllustration, CardsDatabase};
use image::DynamicImage;
use indexmap::IndexMap;
use itertools::Itertools;
use parking_lot::{Condvar, Mutex, RwLock};
use rayon::iter::{
    IntoParallelIterator, IntoParallelRefIterator, ParallelBridge, ParallelIterator,
};
use reqwest::Url;
use scraper::{Html, Selector};
use serde_json::Value;

use crate::{
    DEBUG, PriceCheckMode, http_client,
    images::utils::{dist_hash, to_image_hash},
};

pub fn yuyutei(all_cards: &mut CardsDatabase, mode: PriceCheckMode) {
    println!(
        "Scraping Yuyutei urls... ({})",
        if mode == PriceCheckMode::Images {
            "comparing images"
        } else {
            "quick"
        }
    );

    let scraperapi_key = std::env::var("SCRAPERAPI_API_KEY").ok();
    if scraperapi_key.is_some() {
        println!("using scraperapi.com");
    }

    // handle multiple pages (one page is 600 cards)
    // could be slow when there are multiple pages
    // Process pages in parallel
    let urls = Arc::new(RwLock::new(IndexMap::new()));
    let max_page = Arc::new((Mutex::new(0), Condvar::new()));
    let _ = (1..)
        .par_bridge()
        .map({
            let urls = urls.clone();
            let max_page = max_page.clone();
            move |page| {
                // wait for the max page to be set
                if page > 1 {
                    let mut max_page_lock = max_page.0.lock();
                    if *max_page_lock == 0 {
                        max_page
                            .1
                            .wait_for(&mut max_page_lock, Duration::from_secs(600));
                    } else {
                        max_page.1.notify_all();
                    }

                    if page > *max_page_lock {
                        return None;
                    }
                }

                let mut url = Url::parse("https://yuyu-tei.jp/sell/hocg/s/search").unwrap();
                url.query_pairs_mut()
                    .append_pair("search_word", "")
                    .append_pair("page", page.to_string().as_str());
                let resp = if let Some(scraperapi_key) = &scraperapi_key {
                    http_client()
                        .get("https://api.scraperapi.com/")
                        .query(&[
                            ("api_key", scraperapi_key.as_str()),
                            ("url", url.as_str()),
                            ("session_number", "123"),
                        ])
                        .timeout(Duration::from_secs(70))
                        .send()
                        .unwrap()
                } else {
                    http_client().get(url.clone()).send().unwrap()
                };

                let content = resp.text().unwrap();
                // println!("{content}");

                let document = Html::parse_document(&content);
                let card_lists = Selector::parse("#card-list3").unwrap();
                let rarity_select = Selector::parse("h3 span").unwrap();
                let cards_select = Selector::parse(".card-product").unwrap();
                let number_select = Selector::parse("span").unwrap();
                let name_select = Selector::parse("h4").unwrap();
                let url_select = Selector::parse("a").unwrap();
                let img_select = Selector::parse("img").unwrap();
                let max_page_select =
                    Selector::parse(".pagination li:nth-last-child(2) a").unwrap();

                if let Some(max) = document.select(&max_page_select).next() {
                    let max_page_num = max.text().collect::<String>().parse().unwrap();
                    *max_page.0.lock() = max_page_num;
                }
                max_page.1.notify_all();

                for card_list in document.select(&card_lists) {
                    let rarity: String = card_list
                        .select(&rarity_select)
                        .next()
                        .unwrap()
                        .text()
                        .collect();
                    for card in card_list.select(&cards_select) {
                        let number: String =
                            card.select(&number_select).next().unwrap().text().collect();
                        let name: String =
                            card.select(&name_select).next().unwrap().text().collect();
                        let url_section = card.select(&url_select).next().unwrap();
                        let img_src = url_section.select(&img_select).next().unwrap().attr("src");
                        let url = url_section.attr("href");

                        if name.contains("エラッタ前") {
                            // skip cards with (before errata)
                            continue;
                        }

                        if let (Some(url), Some(img_src)) = (url, img_src) {
                            // group them by url
                            urls.write().entry(url.to_owned()).or_insert((
                                number,
                                rarity.clone(),
                                img_src.to_string(),
                            ));
                        }
                    }
                }

                let max_page_num = *max_page.0.lock();
                println!("Page {page}/{max_page_num} done");
                (page < max_page_num).then_some(())
            }
        })
        .while_some()
        .max(); // Need this to drive the iterator

    let mut urls = Arc::try_unwrap(urls).unwrap().into_inner();
    println!("Found {} Yuyutei urls...", urls.len());

    let mut url_skipped = 0;

    // println!("BEFORE: {urls:#?}");
    // remove existing urls
    let mut existing_urls: HashMap<String, String> = HashMap::new();
    if mode == PriceCheckMode::Quick {
        for card in all_cards
            .values_mut()
            .flat_map(|cs| cs.illustrations.iter_mut())
            .filter(|c| c.yuyutei_sell_url.is_some())
        {
            if let Some(yuyutei_sell_url) = &card.yuyutei_sell_url {
                if urls.shift_remove(yuyutei_sell_url).is_some() {
                    url_skipped += 1;
                }
                // group by image, some entries are duplicated, like hSD01-016
                if let Some(img_path) = card.img_path.japanese.as_deref() {
                    existing_urls
                        .entry(img_path.into())
                        .or_insert(yuyutei_sell_url.clone());
                }
            }
        }
    }

    // swap keys and values
    let urls: HashMap<_, Vec<_>> = urls.into_iter().fold(HashMap::new(), |mut map, (k, v)| {
        // key: (number, rarity), value: (url, img_path)
        map.entry((v.0, v.1)).or_default().push((k, v.2));
        map
    });

    // warn if there are some card with same number and rarity
    for ((number, rarity), urls) in &urls {
        if urls.len() > 1 {
            println!("WARNING: {number} ({rarity}) has multiple urls: {urls:#?}");
        }
    }

    // add the remaining urls
    let existing_urls = Arc::new(RwLock::new(existing_urls));
    let urls = Arc::new(RwLock::new(urls));
    let url_count = Arc::new(Mutex::new(0));
    all_cards.values_mut().par_bridge().for_each(|card| {
        // in quick mode: first come first served. ideal for updating new cards as they are added to the database
        if mode == PriceCheckMode::Quick {
            let illustrations = card
                .illustrations
                .iter_mut()
                .filter(|c| c.yuyutei_sell_url.is_none())
                .collect_vec();

            for illustration in illustrations {
                // first, look for same image
                if let Some(yuyutei_sell_url) = existing_urls.read().get(
                    illustration
                        .img_path
                        .japanese
                        .as_deref()
                        .unwrap_or_default(),
                ) {
                    illustration.yuyutei_sell_url = Some(yuyutei_sell_url.clone());
                } else if let Some(urls) = urls.write().get_mut(&(
                    illustration.card_number.clone(),
                    illustration.rarity.clone(),
                )) {
                    // use the first url
                    if !urls.is_empty() {
                        let (url, _) = urls.swap_remove(0);
                        illustration.yuyutei_sell_url = Some(url.clone());
                        // group by image, some entries are duplicated
                        if let Some(img_path) = illustration.img_path.japanese.as_deref() {
                            existing_urls
                                .write()
                                .entry(img_path.into())
                                .or_insert(url.clone());
                        }
                        *url_count.lock() += 1;
                    }
                }
            }
        // in images mode: find the best match. ideal to fix any issues from quick mode
        } else if mode == PriceCheckMode::Images {
            let rarities = card
                .illustrations
                .iter_mut()
                .map(|c| {
                    // clear any existing url
                    c.yuyutei_sell_url = None;
                    Arc::new(Mutex::new(c))
                })
                .into_group_map_by(|c| c.lock().rarity.clone());

            rarities
                .into_par_iter()
                .for_each(|(rarity, mut illustrations)| {
                    if let Some(urls) = urls.write().get_mut(&(card.card_number.clone(), rarity)) {
                        // nothing to match
                        if urls.is_empty() || illustrations.is_empty() {
                            return;
                        }

                        // only one possible match
                        if urls.len() == 1 && illustrations.len() == 1 {
                            let (url, _) = urls.swap_remove(0);
                            let illust = illustrations.swap_remove(0);
                            illust.lock().yuyutei_sell_url = Some(url.clone());
                            *url_count.lock() += 1;
                            return;
                        }

                        // find the best match, otherwise
                        println!();
                        let images: Vec<_> = urls
                            .par_iter()
                            .map(|(url, img_path)| {
                                // download the image
                                println!("Checking Yuyutei image: {img_path}");
                                let resp = http_client().get(img_path).send().unwrap();
                                let yuyutei_img =
                                    image::load_from_memory(&resp.bytes().unwrap()).unwrap();
                                (url.clone(), yuyutei_img)
                            })
                            .collect();

                        let mut dists: Vec<_> = images
                            .into_iter()
                            .cartesian_product(illustrations.iter())
                            .map(|((url, yuyutei_img), illust)| {
                                let dist = dist_yuyutei_image(&illust.lock(), yuyutei_img);

                                (url, illust, dist)
                            })
                            .collect();

                        // sort by best dist, then update the url
                        dists.sort_by_key(|d| (d.2, d.1.lock().manage_id.japanese));

                        // modify the cards here, to avoid borrowing issue
                        let mut already_set = BTreeMap::new();
                        for (url, illust, dist) in dists {
                            let mut illust = illust.lock();
                            // to handle multiple cards with the same image
                            let min_dist = *already_set.get(&url).unwrap_or(&(u64::MAX));
                            // only one card has the url, no DIST_TOLERANCE
                            if illust.yuyutei_sell_url.is_none() && min_dist >= dist {
                                illust.yuyutei_sell_url = Some(url.clone());
                                urls.retain(|(u, _)| *u != url);
                                already_set.insert(url, dist.min(min_dist));
                                *url_count.lock() += 1;

                                if DEBUG {
                                    // println!(
                                    //     "Updated card {:?} -> manage_id: {}, yuyutei url: {} ({})",
                                    //     illust.card_number,
                                    //     illust.manage_id.unwrap(),
                                    //     illust.yuyutei_sell_url.as_ref().unwrap(),
                                    //     dist
                                    // );
                                }
                            }
                        }
                    }
                });
        }
    });

    // remove empty urls
    let mut urls = Arc::try_unwrap(urls).unwrap().into_inner();
    urls.retain(|_, urls| !urls.is_empty());
    // println!("AFTER: {urls:#?}");

    let url_count = *url_count.lock();
    println!("{url_count} Yuyutei urls updated ({url_skipped} skipped)");
    for ((number, rare), urls) in urls {
        for url in urls {
            let url = url.0;
            println!("NO MATCH: [{number}, {rare}] - {url}");
        }
    }
}

fn dist_yuyutei_image(card: &CardIllustration, yuyutei_img: DynamicImage) -> u64 {
    // compare the image to the card
    println!(
        "Checking Card image: {}",
        card.img_path.japanese.as_deref().unwrap_or_default()
    );

    let h1 = to_image_hash(&yuyutei_img.into_rgb8());
    let h2 = &card.img_hash;

    let dist = dist_hash(&h1, h2);

    if DEBUG {
        println!("Yuyutei hash: {h1}");
        println!("Card hash: {h2}");
        println!("Distance: {dist}");
    }

    dist
}

pub fn tcgplayer(all_cards: &mut CardsDatabase, mode: PriceCheckMode) {
    println!(
        "Scraping TCGplayer product ids... ({})",
        if mode == PriceCheckMode::Images {
            "comparing images"
        } else {
            "quick"
        }
    );

    const HOCG_CATEGORY_ID: &str = "87";

    println!("Fetching TCGplayer groups...");
    let groups_url = format!("https://tcgcsv.com/tcgplayer/{HOCG_CATEGORY_ID}/groups");
    let resp = http_client().get(&groups_url).send().unwrap();

    let all_groups = resp.json::<Value>().unwrap();
    let all_groups = all_groups["results"].as_array().unwrap();
    println!("Found {} TCGplayer groups.", all_groups.len());

    let mut product_ids = IndexMap::new();
    for group in all_groups {
        let group_name = group["name"].as_str().unwrap_or("Unknown Group");
        let group_id = group["groupId"].as_u64().unwrap();
        println!("Processing group: {group_name}");

        // Fetch products
        let products_url =
            format!("https://tcgcsv.com/tcgplayer/{HOCG_CATEGORY_ID}/{group_id}/products");
        let resp = http_client().get(&products_url).send().unwrap();

        #[derive(serde::Deserialize, Debug)]
        #[serde(rename_all = "camelCase")]
        struct TcgPlayerProduct {
            product_id: u32,
            name: String,
            image_url: String,
            extended_data: Vec<Value>,
        }

        let products = resp.json::<Value>().unwrap();
        let products = products["results"].as_array().unwrap();
        for product in products {
            let Ok(product): Result<TcgPlayerProduct, _> = serde_json::from_value(product.clone())
                .inspect_err(|e| {
                    println!("Failed to parse product: {e}");
                })
            else {
                continue;
            };
            if product.extended_data.is_empty() {
                // skip booster boxes and other products without extended data
                continue;
            }

            let card_number = product
                .extended_data
                .iter()
                .find(|ed| ed["name"] == "Number")
                .and_then(|ed| ed["value"].as_str())
                .unwrap_or("Unknown Number");
            let rarity = product
                .extended_data
                .iter()
                .find(|ed| ed["name"] == "Rarity")
                .and_then(|ed| ed["value"].as_str())
                .map(|r| match r.to_lowercase().as_str() {
                    "common" => "C",
                    "uncommon" => "U",
                    "rare" => "R",
                    "double rare" => "RR",
                    "super rare" => "SR",
                    "ultra rare" => "UR",
                    "secret rare" => "SEC",
                    "special" => "S",
                    "oshi super rare" => "OSR",
                    "oshi ultra rare" => "OUR",
                    "oshi common" => "OC",
                    "promo" => "P",
                    _ => r,
                })
                .unwrap_or("Unknown Rarity");
            if DEBUG {
                println!(
                    "  Product: {} - {} - {} - {}",
                    product.product_id, product.name, card_number, rarity
                );
            }

            product_ids.entry(product.product_id).or_insert((
                card_number.to_string(),
                rarity.to_string(),
                product.image_url.to_string(),
            ));
        }
    }

    println!("Found {} TCGplayer products...", product_ids.len());

    let mut product_ids_skipped = 0;

    // println!("BEFORE: {urls:#?}");
    // remove existing product ids
    let mut existing_ids: HashMap<String, u32> = HashMap::new();
    if mode == PriceCheckMode::Quick {
        for card in all_cards
            .values_mut()
            .flat_map(|cs| cs.illustrations.iter_mut())
            .filter(|c| c.tcgplayer_product_id.is_some())
        {
            if let Some(tcgplayer_product_id) = &card.tcgplayer_product_id {
                if product_ids.shift_remove(tcgplayer_product_id).is_some() {
                    product_ids_skipped += 1;
                }
                // group by image, some entries are duplicated, like hSD01-016
                if let Some(img_path) = card.img_path.english.as_deref() {
                    existing_ids
                        .entry(img_path.into())
                        .or_insert(*tcgplayer_product_id);
                }
            }
        }
    }

    // swap keys and values
    let product_ids: HashMap<_, Vec<_>> =
        product_ids
            .into_iter()
            .fold(HashMap::new(), |mut map, (k, v)| {
                // key: (number, rarity), value: (product_id, img_path)
                map.entry((v.0, v.1)).or_default().push((k, v.2));
                map
            });

    // warn if there are some card with same number and rarity
    for ((number, rarity), urls) in &product_ids {
        if urls.len() > 1 {
            println!("WARNING: {number} ({rarity}) has multiple urls: {urls:#?}");
        }
    }

    // add the remaining urls
    let existing_ids = Arc::new(RwLock::new(existing_ids));
    let product_ids = Arc::new(RwLock::new(product_ids));
    let product_ids_count = Arc::new(Mutex::new(0));
    all_cards.values_mut().par_bridge().for_each(|card| {
        // in quick mode: first come first served. ideal for updating new cards as they are added to the database
        if mode == PriceCheckMode::Quick {
            let illustrations = card
                .illustrations
                .iter_mut()
                .filter(|c| c.tcgplayer_product_id.is_none())
                .collect_vec();

            for illustration in illustrations {
                // first, look for same image
                if let Some(tcgplayer_product_id) = existing_ids
                    .read()
                    .get(illustration.img_path.english.as_deref().unwrap_or_default())
                {
                    illustration.tcgplayer_product_id = Some(*tcgplayer_product_id);
                } else if let Some(product_ids) = product_ids.write().get_mut(&(
                    illustration.card_number.clone(),
                    illustration.rarity.clone(),
                )) {
                    // use the first url
                    if !product_ids.is_empty() {
                        let (product_id, _) = product_ids.swap_remove(0);
                        illustration.tcgplayer_product_id = Some(product_id);
                        // group by image, some entries are duplicated
                        if let Some(img_path) = illustration.img_path.english.as_deref() {
                            existing_ids
                                .write()
                                .entry(img_path.into())
                                .or_insert(product_id);
                            *product_ids_count.lock() += 1;
                        }
                    }
                }
            }
        // in images mode: find the best match. ideal to fix any issues from quick mode
        } else if mode == PriceCheckMode::Images {
            let rarities = card
                .illustrations
                .iter_mut()
                .map(|c| {
                    // clear any existing url
                    c.tcgplayer_product_id = None;
                    Arc::new(Mutex::new(c))
                })
                .into_group_map_by(|c| c.lock().rarity.clone());

            rarities
                .into_par_iter()
                .for_each(|(rarity, mut illustrations)| {
                    if let Some(product_ids) = product_ids
                        .write()
                        .get_mut(&(card.card_number.clone(), rarity))
                    {
                        // nothing to match
                        if product_ids.is_empty() || illustrations.is_empty() {
                            return;
                        }

                        // only one possible match
                        if product_ids.len() == 1 && illustrations.len() == 1 {
                            let (product_id, _) = product_ids.swap_remove(0);
                            let illust = illustrations.swap_remove(0);
                            illust.lock().tcgplayer_product_id = Some(product_id);
                            *product_ids_count.lock() += 1;
                            return;
                        }

                        // find the best match, otherwise
                        println!();
                        let images: Vec<_> = product_ids
                            .par_iter()
                            .map(|(product_id, img_path)| {
                                // download the image
                                println!("Checking TCGplayer image: {img_path}");
                                let resp = http_client().get(img_path).send().unwrap();
                                let tcgplayer_img =
                                    image::load_from_memory(&resp.bytes().unwrap()).unwrap();
                                (*product_id, tcgplayer_img)
                            })
                            .collect();

                        let mut dists: Vec<_> = images
                            .into_iter()
                            .cartesian_product(illustrations.iter())
                            .map(|((product_id, tcgplayer_img), illust)| {
                                let dist = dist_tcgplayer_image(&illust.lock(), tcgplayer_img);

                                (product_id, illust, dist)
                            })
                            .collect();

                        // sort by best dist, then update the url
                        dists.sort_by_key(|d| (d.2, d.1.lock().manage_id.english));

                        // modify the cards here, to avoid borrowing issue
                        let mut already_set = BTreeMap::new();
                        for (product_id, illust, dist) in dists {
                            let mut illust = illust.lock();
                            // to handle multiple cards with the same image
                            let min_dist = *already_set.get(&product_id).unwrap_or(&(u64::MAX));
                            // only one card has the url, no DIST_TOLERANCE
                            if illust.tcgplayer_product_id.is_none() && min_dist >= dist {
                                illust.tcgplayer_product_id = Some(product_id);
                                product_ids.retain(|(u, _)| *u != product_id);
                                already_set.insert(product_id, dist.min(min_dist));
                                *product_ids_count.lock() += 1;

                                if DEBUG {
                                    // println!(
                                    //     "Updated card {:?} -> manage_id: {}, tcgplayer url: {} ({})",
                                    //     illust.card_number,
                                    //     illust.manage_id.unwrap(),
                                    //     illust.tcgplayer_product_id.as_ref().unwrap(),
                                    //     dist
                                    // );
                                }
                            }
                        }
                    }
                });
        }
    });

    // remove empty product ids
    let mut product_ids = Arc::try_unwrap(product_ids).unwrap().into_inner();
    product_ids.retain(|_, product_ids| !product_ids.is_empty());
    // println!("AFTER: {urls:#?}");

    let product_id_count = *product_ids_count.lock();
    println!("{product_id_count} TCGplayer product ids updated ({product_ids_skipped} skipped)");
    for ((number, rare), product_ids) in product_ids {
        for product_id in product_ids {
            let product_id = product_id.0;
            println!("NO MATCH: [{number}, {rare}] - {product_id}");
        }
    }
}

fn dist_tcgplayer_image(card: &CardIllustration, tcgplayer_img: DynamicImage) -> u64 {
    // compare the image to the card
    println!(
        "Checking Card image: {}",
        card.img_path.english.as_deref().unwrap_or_default()
    );

    let h1 = to_image_hash(&tcgplayer_img.into_rgb8());
    let h2 = &card.img_hash;

    let dist = dist_hash(&h1, h2);

    if DEBUG {
        println!("TCGplayer hash: {h1}");
        println!("Card hash: {h2}");
        println!("Distance: {dist}");
    }

    dist
}
