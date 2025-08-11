use std::{
    collections::{BTreeMap, HashMap},
    error::Error,
    fs::{self, File},
    io::{Read, Write},
    path::Path,
    sync::{Arc, atomic::AtomicU32},
};

use hocg_fan_sim_assets_model::{CardIllustration, CardsDatabase, Language, Localized};
use image::DynamicImage;
use itertools::Itertools;
use oxipng::{InFile, Options, OutFile};
use parking_lot::{Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};
use rayon::iter::{IntoParallelIterator, IntoParallelRefIterator, ParallelIterator};
use reqwest::header::{LAST_MODIFIED, REFERER};
use scraper::{Html, Selector};
use std::io::Cursor;
use walkdir::WalkDir;
use webp::{Encoder, WebPMemory};
use zip::write::SimpleFileOptions;

use crate::{
    DEBUG, http_client,
    images::utils::{DIST_TOLERANCE, dist_hash, path_to_image_hash, to_image_hash},
};

static WEBP_QUALITY: f32 = 80.0;

pub fn download_images(
    filtered_cards: &[(String, usize)],
    images_jp_path: &Path,
    images_en_path: &Path,
    all_cards: &mut CardsDatabase,
    force_download: bool,
    optimized_original_images: bool,
    language: Language,
) {
    println!(
        "Downloading {} {:?} images...",
        filtered_cards.len(),
        language
    );

    let images_path = match language {
        Language::Japanese => images_jp_path,
        Language::English => images_en_path,
    };

    let all_cards = Arc::new(RwLock::new(all_cards));
    let image_count = AtomicU32::new(0);
    let image_skipped = AtomicU32::new(0);

    filtered_cards.par_iter().for_each({
        let all_cards = all_cards.clone();
        let image_count = &image_count;
        let image_skipped = &image_skipped;
        move |(card_number, illust_idx)| {
            // https://hololive-official-cardgame.com/wp-content/images/cardlist/hSD01/hSD01-006_RR.png

            let img_last_modified;
            // scope for the read guard
            {
                let card = RwLockReadGuard::map(all_cards.read(), |ac| {
                    ac.get(card_number)
                        .unwrap()
                        .illustrations
                        .get(*illust_idx)
                        .unwrap()
                });

                // skip unreleased cards
                if card.manage_id.value(language).is_none() {
                    return;
                }

                let Some(img_path) = card.img_path.value(language) else {
                    eprintln!(
                        "Skipping card {card_number} illustration {illust_idx} without image path"
                    );
                    return;
                };

                let (url, referrer) = match language {
                    Language::Japanese => (
                        "https://hololive-official-cardgame.com/wp-content/images/cardlist/",
                        "https://hololive-official-cardgame.com/",
                    ),
                    Language::English => (
                        "https://en.hololive-official-cardgame.com/wp-content/images/cardlist/",
                        "https://en.hololive-official-cardgame.com/",
                    ),
                };

                // check if it's a new image
                let resp = http_client()
                    .head(format!("{url}{}", img_path.replace(".webp", ".png")))
                    .header(REFERER, referrer)
                    .send()
                    .unwrap();

                let last_modified = resp
                    .headers()
                    .get(LAST_MODIFIED)
                    .and_then(|h| h.to_str().ok());

                // is it a new image?
                let last_modified_time = last_modified.map(httpdate::parse_http_date);
                let card_last_modified_time = card
                    .img_last_modified
                    .value(language)
                    .as_deref()
                    .map(httpdate::parse_http_date);
                if let (Some(Ok(last_modified_time)), Some(Ok(card_last_modified_time))) =
                    (last_modified_time, card_last_modified_time)
                    && last_modified_time <= card_last_modified_time
                    && !force_download
                {
                    // we already have the image
                    image_skipped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    return;
                }

                img_last_modified = last_modified
                    .map(String::from)
                    .or(card.img_last_modified.value(language).clone());

                // download the image
                let Ok(resp) = http_client()
                    .get(format!("{url}{}", img_path.replace(".webp", ".png")))
                    .header(REFERER, referrer)
                    .send()
                    .inspect_err(|e| eprintln!("Error downloading image {img_path:?}: {e}"))
                else {
                    return;
                };

                // Using `image` crate, open the included .jpg file
                let Ok(img) = image::load_from_memory(&resp.bytes().unwrap()).inspect_err(|e| {
                    eprintln!("Error loading image {img_path:?}: {e}");
                }) else {
                    return;
                };

                if optimized_original_images {
                    let path = images_path.join(img_path);
                    if let Some(parent) = Path::new(&path).parent() {
                        fs::create_dir_all(parent).unwrap();
                    }
                    img.save(&path).unwrap();

                    // optimize the image
                    oxipng::optimize(
                        &InFile::from(&path),
                        &OutFile::from_path(path),
                        &Options::default(),
                    )
                    .unwrap();
                } else {
                    // Create the WebP encoder for the above image
                    let encoder: Encoder = Encoder::from_image(&img).unwrap();
                    // Encode the image at a specified quality 0-100
                    let webp: WebPMemory = encoder.encode(WEBP_QUALITY);
                    // Define and write the WebP-encoded file to a given path
                    let path = images_path.join(img_path.replace(".png", ".webp"));
                    if let Some(parent) = Path::new(&path).parent() {
                        fs::create_dir_all(parent).unwrap();
                    }
                    std::fs::write(&path, &*webp).unwrap();
                }

                image_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let image_count = image_count.load(std::sync::atomic::Ordering::Relaxed);
                let image_skipped = image_skipped.load(std::sync::atomic::Ordering::Relaxed);
                if image_count % 10 == 0 {
                    println!("{image_count} images downloaded ({image_skipped} skipped)");
                }
            }

            // scope for write guard
            let mut card = RwLockWriteGuard::map(all_cards.write(), |ac| {
                ac.get_mut(card_number)
                    .unwrap()
                    .illustrations
                    .get_mut(*illust_idx)
                    .unwrap()
            });
            *card.img_last_modified.value_mut(language) = img_last_modified;
            // update image hash
            card.img_hash = path_to_image_hash(
                &images_path.join(card.img_path.value(language).as_deref().unwrap()),
            );
        }
    });

    let image_count = image_count.load(std::sync::atomic::Ordering::Relaxed);
    let image_skipped = image_skipped.load(std::sync::atomic::Ordering::Relaxed);
    println!("{image_count} images downloaded ({image_skipped} skipped)");
}

pub fn prepare_en_proxy_images(
    images_en_path: &Path,
    all_cards: &mut CardsDatabase,
    proxy_path: &Path,
) {
    if !proxy_path.is_dir() {
        panic!("proxy_path should be dir");
    }

    println!(
        "Preparing {} proxy images...",
        all_cards
            .values()
            .flat_map(|c| c.illustrations.iter())
            .filter(|c| c.manage_id.english.is_none())
            .count()
    );

    // key: (number), value: (file_name, img_path)
    let mut files: HashMap<_, Vec<_>> = HashMap::new();

    for entry in WalkDir::new(proxy_path).into_iter().flatten() {
        // ignore blanks
        if !entry
            .path()
            .ancestors()
            .filter_map(|p| p.file_name())
            .any(|p| {
                matches!(
                    p.to_ascii_lowercase().to_str(),
                    Some("blanks") | Some("blank")
                )
            })
            && entry.path().is_file()
            && entry.path().extension() == Some("png".as_ref())
            && let Some(file_stem) = entry.path().file_stem()
        {
            let Some((card_number, part)) = file_stem.to_str().unwrap().split_once('_') else {
                eprintln!(
                    "Skipping proxy image without card number: {}",
                    entry.path().display()
                );
                continue;
            };
            let rarity = if let Some((rarity, _)) = part.split_once('_') {
                rarity
            } else {
                part
            };

            let paths = files.entry(card_number.to_owned()).or_default();
            if paths.iter().any(|(f, _, _)| *f == file_stem) {
                if DEBUG {
                    println!("Skipping duplicate proxy image: {}", entry.path().display());
                }
            } else {
                // add the file name and path
                paths.push((
                    file_stem.to_owned(),
                    rarity.to_owned(),
                    entry.path().to_owned(),
                ));
            }
        }
    }

    let proxies = Arc::new(RwLock::new(files));
    let proxies_count = AtomicU32::new(0);
    let proxies_skipped = AtomicU32::new(0);

    let rarities = all_cards
        .values_mut()
        .flat_map(|c| c.illustrations.iter_mut())
        .map(|c| {
            // clear any existing proxy, keep official images
            if c.manage_id.english.is_none() {
                c.img_path.english = None;
            }
            Arc::new(Mutex::new(c))
        })
        .into_group_map_by(|c| {
            let c = c.lock();
            c.card_number.clone()
        });

    rarities
        .into_par_iter()
        .for_each(|(card_number, mut illustrations)| {
            if let Some(img_paths) = proxies.write().get_mut(&(card_number.clone())) {
                // nothing to match
                if illustrations.is_empty() {
                    return;
                }
                // missing proxies
                if img_paths.is_empty() {
                    proxies_skipped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    return;
                }

                // only one possible match
                if img_paths.len() == 1 && illustrations.len() == 1 {
                    let (_file_name, _rarity, img_path) = img_paths.swap_remove(0);
                    let illust = illustrations.swap_remove(0);
                    let mut illust = illust.lock();
                    if illust.manage_id.english.is_none() {
                        illust.img_path.english =
                            Some(save_proxy_image(&illust, &img_path, images_en_path));

                        if DEBUG {
                            println!(
                                "Updated card {:?} -> manage_id: {:?}, proxy path: {}",
                                illust.card_number,
                                illust.manage_id.japanese,
                                illust.img_path.english.as_ref().unwrap(),
                            );
                        }
                    }
                    proxies_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    return;
                }

                // find the best match, otherwise
                if DEBUG {
                    println!();
                }
                let proxies: Vec<_> = img_paths
                    .par_iter()
                    .map(|(_, rarity, img_path)| {
                        // download the image
                        println!("Checking proxy image: {}", img_path.display());
                        let proxy_img =
                            image::load_from_memory(&fs::read(img_path).unwrap()).unwrap();
                        (rarity.to_owned(), img_path.to_owned(), proxy_img)
                    })
                    .collect();

                let mut dists: Vec<_> = proxies
                    .into_iter()
                    .cartesian_product(illustrations.iter())
                    .map(|((rarity, img_path, proxy_img), illust)| {
                        let dist = dist_proxy_image(&illust.lock(), proxy_img);

                        (rarity, img_path, illust, dist)
                    })
                    .collect();

                // sort by best dist, then update the proxy (maybe need adjusting)
                dists.sort_by_cached_key(|d| {
                    let rarity = &d.0;
                    let dist = d.3;
                    let illust = d.2.lock();
                    // have rarity and release date influence the matching
                    let rarity_rank = if *rarity != illust.rarity {
                        DIST_TOLERANCE
                    } else {
                        0
                    };
                    let id_rank = illust.manage_id.japanese.unwrap_or(u32::MAX) as u64;
                    let dist_rank = dist.saturating_div(DIST_TOLERANCE);
                    rarity_rank
                        .saturating_add(id_rank)
                        .saturating_add(dist_rank)
                });

                // modify the cards here, to avoid borrowing issue
                let mut already_set = BTreeMap::new();
                for (_rarity, img_path, illust, dist) in dists {
                    let mut illust = illust.lock();

                    // only one card has the proxy, no DIST_TOLERANCE
                    if already_set.contains_key(&img_path) {
                        continue;
                    }

                    if illust.img_path.english.is_none() || illust.manage_id.english.is_some() {
                        if illust.manage_id.english.is_none() {
                            illust.img_path.english =
                                Some(save_proxy_image(&illust, &img_path, images_en_path));

                            if DEBUG {
                                println!(
                                    "Updated card {:?} -> manage_id: {:?}, proxy path: {} ({})",
                                    illust.card_number,
                                    illust.manage_id.japanese,
                                    illust.img_path.english.as_ref().unwrap(),
                                    dist
                                );
                            }
                        }
                        img_paths.retain(|(_, _, p)| *p != img_path);
                        already_set.insert(img_path, dist);
                        proxies_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
        });

    // remove empty proxies
    let mut proxies = Arc::try_unwrap(proxies).unwrap().into_inner();
    proxies.retain(|_, proxies| !proxies.is_empty());
    // println!("AFTER: {proxies:#?}");

    let proxies_count = proxies_count.load(std::sync::atomic::Ordering::Relaxed);
    let proxies_skipped = proxies_skipped.load(std::sync::atomic::Ordering::Relaxed);
    println!("{proxies_count} Proxies updated ({proxies_skipped} skipped)");
    for (number, proxies) in proxies {
        for proxy in proxies {
            let rare = proxy.1;
            let proxy = proxy.0;
            println!("NO MATCH: [{number}, {rare}] - {}", proxy.display());
        }
    }
}

fn save_proxy_image(card: &CardIllustration, img_path: &Path, images_en_path: &Path) -> String {
    const PROXIES_FOLDER: &str = "proxies";

    // Using `image` crate, open the included .jpg file
    let img = image::load_from_memory(&fs::read(img_path).unwrap()).unwrap();

    // Create the WebP encoder for the above image
    let encoder: Encoder = Encoder::from_image(&img).unwrap();
    // Encode the image at a specified quality 0-100
    let webp: WebPMemory = encoder.encode(WEBP_QUALITY);
    // Define and write the WebP-encoded file to a given path
    let img_en_proxy = Path::new(PROXIES_FOLDER).join(
        card.img_path
            .japanese
            .as_deref()
            .unwrap_or_default()
            .replace(".png", ".webp"),
    );
    let path = images_en_path.join(&img_en_proxy);
    if let Some(parent) = Path::new(&path).parent() {
        fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, &*webp).unwrap();

    img_en_proxy
        .to_str()
        .unwrap()
        .replace("\\", "/")
        .to_string()
}

fn dist_proxy_image(card: &CardIllustration, proxy_img: DynamicImage) -> u64 {
    // compare the image to the card
    if DEBUG {
        println!(
            "Checking Card image: {}",
            card.img_path.japanese.as_deref().unwrap_or_default()
        );
    }

    let h1 = to_image_hash(&proxy_img.into_rgb8());
    let h2 = &card.img_hash;

    let dist = dist_hash(&h1, h2);

    if DEBUG {
        println!("Proxy hash: {h1}");
        println!("Card hash: {h2}");
        println!("Distance: {dist}");
    }

    dist
}

pub fn download_images_from_ogbajoj_sheet(
    images_jp_path: &Path,
    images_en_path: &Path,
    all_cards: &mut CardsDatabase,
) {
    const UNRELEASED_FOLDER: &str = "unreleased";
    const SPREADSHEET_ID: &str = "1IdaueY-Jw8JXjYLOhA9hUd2w0VRBao9Z1URJwmCWJ64";

    println!("Downloading unreleased images from @ogbajoj's sheet...");

    let all_cards = Arc::new(RwLock::new(all_cards));

    // Download the entire spreadsheet as HTML zip
    let url = format!("https://docs.google.com/spreadsheets/d/{SPREADSHEET_ID}/export?format=zip");
    let resp = http_client().get(url).send().unwrap();
    let bytes = resp.bytes().unwrap();
    let cursor = Cursor::new(bytes.to_vec());

    let archive = Arc::new(RwLock::new(zip::ZipArchive::new(cursor).unwrap()));

    // Precompute selectors
    let table_sel = Selector::parse("table").unwrap();
    let tr_sel = Selector::parse("tr").unwrap();
    let td_sel = Selector::parse("td").unwrap();
    let img_sel = Selector::parse("img").unwrap();

    // Image URLs are external; fetch via HTTP
    fn download_image(
        url: &str,
        row_idx: usize,
        set_code: &str,
        name: &str,
    ) -> Result<DynamicImage, Box<dyn Error>> {
        let resp = http_client()
            .get(url)
            .header(REFERER, "https://docs.google.com/")
            .send()
            .inspect_err(|e| {
                eprintln!("[{name}] row {row_idx} {set_code}: request error: {e}");
            })?;

        if !resp.status().is_success() {
            eprintln!(
                "[{name}] row {row_idx} {set_code}: HTTP status {} for {url}",
                resp.status()
            );
            return Err(Box::from(format!("HTTP error: {}", resp.status())));
        }
        let img_bytes = resp.bytes().inspect_err(|e| {
            eprintln!("[{name}] row {row_idx} {set_code}: read body error: {e}");
        })?;

        // Decode image
        Ok(image::load_from_memory(&img_bytes).inspect_err(|e| {
            eprintln!("[{name}] row {row_idx} {set_code}: decode error: {e}");
        })?)
    }

    // Iterate html files inside the ZIP
    let archive_len = { archive.read().len() };
    (0..archive_len)
        .into_par_iter()
        .filter_map({
            let archive = archive.clone();
            move |i| {
                let mut archive = archive.write();
                let mut file = archive.by_index(i).unwrap();
                let name = file.name().to_string();
                if !name.to_ascii_lowercase().ends_with(".html") {
                    return None;
                }

                // Read HTML content
                let mut html = String::new();
                file.read_to_string(&mut html).unwrap();

                Some((name, html))
            }
        })
        .for_each({
            let all_cards = all_cards.clone();
            move |(name, html)| {
                let mut imported = 0;
                let mut skipped = 0;

                println!("[{name}] Reading HTML...");

                let document = Html::parse_document(&html);
                for table in document.select(&table_sel) {
                    // Build header index map from the first tbody row of <td> labels
                    let trs = table.select(&tr_sel).collect_vec();
                    let mut set_code_idx: Option<usize> = None;
                    let mut image_idx: Option<usize> = None;
                    let mut rarity_1_idx: Option<usize> = None;
                    let mut alternate_art_idx: Option<usize> = None;
                    let mut rarity_2_idx: Option<usize> = None;
                    let mut text_idx = None;
                    let mut header_row_idx: Option<usize> = None;

                    for (i, header_tr) in trs.iter().enumerate() {
                        set_code_idx = None;
                        image_idx = None;
                        rarity_1_idx = None;
                        alternate_art_idx = None;
                        rarity_2_idx = None;
                        text_idx = None;

                        let headers = header_tr
                            .select(&td_sel)
                            .map(|td| td.text().collect::<String>().trim().to_ascii_lowercase())
                            .collect_vec();
                        for (idx, h) in headers.iter().enumerate() {
                            if h.contains("setcode") || h.contains("set code") {
                                set_code_idx = Some(idx);
                            }
                            if h.contains("image") {
                                image_idx = Some(idx);
                            }
                            if h.contains("alternate art") {
                                alternate_art_idx = Some(idx);
                            }
                            if h.contains("rarity") {
                                if rarity_1_idx.is_none() {
                                    // first rarity column
                                    rarity_1_idx = Some(idx);
                                } else if rarity_2_idx.is_none() {
                                    // second rarity column
                                    rarity_2_idx = Some(idx);
                                }
                            }
                            if h.contains("text") {
                                text_idx = Some(idx);
                            }
                        }

                        if set_code_idx.is_some() && image_idx.is_some() && rarity_1_idx.is_some() {
                            header_row_idx = Some(i);

                            if DEBUG {
                                println!(
                                    "Header indices -> setcode: {:?}, image: {:?}, rarity: {:?}",
                                    set_code_idx, image_idx, rarity_1_idx
                                );
                            }
                            break;
                        }
                    }

                    // If we don't find headers, skip this table
                    let Some(header_row_idx) = header_row_idx else {
                        println!("[{name}]: missing header row");
                        continue;
                    };

                    // Process data rows after the header row
                    for (row_idx, tr) in trs.into_iter().enumerate().skip(header_row_idx + 1) {
                        let tds = tr.select(&td_sel).collect_vec();
                        if tds.is_empty()
                            || tds
                                .iter()
                                .all(|td| td.text().collect::<String>().trim().is_empty())
                        {
                            continue;
                        }

                        let set_code = set_code_idx
                            .and_then(|idx| tds.get(idx))
                            .map(|td| td.text().collect::<String>().trim().to_string())
                            .unwrap_or_default();
                        if set_code.is_empty() {
                            if DEBUG {
                                println!("[{name}] row {row_idx}: missing set code");
                            }
                            continue;
                        }

                        let text = text_idx
                            .and_then(|idx| tds.get(idx))
                            .map(|td| td.text().collect::<String>().trim().to_string())
                            .unwrap_or_default();
                        // language switch for English exclusive cards like hY01-008
                        let language = if text.contains("EN exclusive") {
                            Language::English
                        } else {
                            Language::Japanese
                        };
                        let images_path = match language {
                            Language::Japanese => images_jp_path,
                            Language::English => images_en_path,
                        };

                        // Collect image sources and rarities
                        let mut images = Vec::with_capacity(2);

                        let img_src = image_idx
                            .and_then(|idx| tds.get(idx))
                            .and_then(|td| td.select(&img_sel).next())
                            .and_then(|img| img.value().attr("src"))
                            .map(|s| s.to_string());
                        let rarity_1 = rarity_1_idx
                            .and_then(|idx| tds.get(idx))
                            .map(|td| td.text().collect::<String>().trim().to_string());
                        if let Some(img_src) = img_src
                            && let Some(rarity) = rarity_1
                        {
                            images.push((img_src, rarity));
                        }

                        let rarity_2 = rarity_2_idx
                            .and_then(|idx| tds.get(idx))
                            .map(|td| td.text().collect::<String>().trim().to_string())
                            .filter(|r| r != "SY"); // Cheers have a different number
                        let alternate_art_src = alternate_art_idx
                            .and_then(|idx| tds.get(idx))
                            .and_then(|td| td.select(&img_sel).next())
                            .and_then(|img| img.value().attr("src"))
                            .map(|s| s.to_string());
                        if let Some(alt_art_src) = alternate_art_src
                            && let Some(rarity) = rarity_2
                        {
                            images.push((alt_art_src, rarity));
                        }

                        for (img_src, rarity) in images {
                            if DEBUG {
                                println!("[{name}] row: {set_code} -> {img_src}");
                            }

                            // one image for hashing, the other for display
                            let full_size_img_url = img_src
                                .find("=")
                                .map(|idx| &img_src[..idx])
                                .unwrap_or(&img_src);
                            let hash_img_ratio = 3;
                            let hash_img_w = 40 * hash_img_ratio;
                            let hash_img_h = 56 * hash_img_ratio;
                            let hash_img_url =
                                format!("{full_size_img_url}=w{hash_img_w}-h{hash_img_h}");

                            // Download and decode image
                            let Ok(hash_img) =
                                download_image(&hash_img_url, row_idx, &set_code, &name)
                            else {
                                continue;
                            };
                            let img_hash = to_image_hash(&hash_img.into_rgb8());

                            let mut _adding = false;
                            let mut img_unreleased = Path::new(UNRELEASED_FOLDER)
                                .join(format!("{}_{}.webp", set_code, rarity));
                            {
                                // Find the card or create
                                let mut all_cards = all_cards.write();
                                let card = all_cards.entry(set_code.clone()).or_default();

                                // find a matching illustration, otherwise create a new one
                                let mut matching_illustrations = card
                                    .illustrations
                                    .iter_mut()
                                    .filter(|i| {
                                        i.card_number == set_code
                                            && i.rarity.eq_ignore_ascii_case(&rarity)
                                    })
                                    .map(|i| (dist_hash(&i.img_hash, &img_hash), i))
                                    // more tolerance for manual cropping
                                    .filter(|(dist, _)| *dist <= DIST_TOLERANCE * 32)
                                    .collect_vec();
                                matching_illustrations.sort_by_key(|(dist, _)| *dist);

                                let illust =
                                    if let Some((_, illust)) = matching_illustrations.first_mut() {
                                        // Use existing unreleased illustration, with a different image
                                        // Master Sheet has duplicate entries
                                        if !illust.manage_id.has_value()
                                            && illust.img_hash != img_hash
                                            && name != "Master Sheet.html"
                                        {
                                            _adding = false;

                                            if let Some(img_path) =
                                                illust.img_path.value_mut(language).as_mut()
                                            {
                                                img_unreleased = Path::new(img_path).into();
                                            }

                                            illust
                                        } else {
                                            // already exists
                                            skipped += 1;
                                            continue;
                                        }
                                    } else {
                                        _adding = true;

                                        // find a new image file name
                                        let mut counter = 2;
                                        while card.illustrations.iter().any(|i| {
                                            i.img_path
                                                .value(language)
                                                .as_ref()
                                                .map(|p| {
                                                    *p == img_unreleased
                                                        .to_str()
                                                        .unwrap()
                                                        .replace("\\", "/")
                                                })
                                                .unwrap_or(false)
                                        }) {
                                            img_unreleased = Path::new(UNRELEASED_FOLDER).join(
                                                format!("{}_{}_{}.webp", set_code, rarity, counter),
                                            );
                                            counter += 1;
                                        }

                                        // Doesn't exist, add illustration
                                        card.illustrations.push(CardIllustration {
                                            card_number: set_code.clone(),
                                            rarity: rarity.clone(),
                                            img_path: Localized::new(
                                                language,
                                                img_unreleased
                                                    .to_str()
                                                    .unwrap()
                                                    .replace("\\", "/")
                                                    .to_string(),
                                            ),
                                            ..Default::default()
                                        });
                                        card.illustrations.last_mut().unwrap()
                                    };

                                // could be cleared on errors, with path
                                illust.img_hash = img_hash.clone();
                            }

                            // Download and decode image
                            let mut error = false;
                            if let Ok(full_size_img) =
                                download_image(full_size_img_url, row_idx, &set_code, &name)
                            {
                                // Resize image
                                let resized_img = full_size_img.resize_exact(
                                    400,
                                    559,
                                    image::imageops::FilterType::Lanczos3,
                                );
                                // Create the WebP encoder for the above image
                                let encoder: Encoder = Encoder::from_image(&resized_img).unwrap();
                                // Encode the image at a specified quality 0-100
                                let webp: WebPMemory = encoder.encode(WEBP_QUALITY);
                                // Define and write the WebP-encoded file to a given path
                                let path = images_path.join(&img_unreleased);
                                if let Some(parent) = Path::new(&path).parent() {
                                    fs::create_dir_all(parent).unwrap();
                                }
                                std::fs::write(&path, &*webp).unwrap();
                            } else {
                                error = true;
                            }

                            if error {
                                let mut all_cards = all_cards.write();
                                let illust = all_cards
                                    .get_mut(&set_code)
                                    .unwrap()
                                    .illustrations
                                    .iter_mut()
                                    .find(|i| i.rarity == rarity && i.img_hash == img_hash)
                                    .unwrap();
                                // could not download image
                                illust.img_hash = Default::default();
                                *illust.img_path.value_mut(language) = None;
                            }

                            if DEBUG {
                                println!(
                                    "{} {set_code} [{rarity}] -> {}",
                                    if _adding { "Added" } else { "Updated" },
                                    img_unreleased.display()
                                );
                            }

                            imported += 1;
                        }
                    }
                }

                println!("[{name}] Imported {imported} images from sheet ({skipped} skipped)");
            }
        });
}

pub fn zip_images(file_name: &str, assets_path: &Path, images_path: &Path) {
    let file_path = assets_path.join(file_name).with_extension("zip");
    let file = File::create(&file_path).unwrap();

    let mut zip = zip::ZipWriter::new(file);
    let options = SimpleFileOptions::default();

    let prefix = images_path;
    let mut buffer = Vec::new();
    for entry in WalkDir::new(images_path).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        let name = path.strip_prefix(prefix).unwrap();

        // Write file or directory explicitly
        // Some unzip tools unzip files with directory paths correctly, some do not!
        if path.is_file() {
            zip.start_file_from_path(name, options).unwrap();
            let mut f = File::open(path).unwrap();

            f.read_to_end(&mut buffer).unwrap();
            zip.write_all(&buffer).unwrap();
            buffer.clear();
        } else if !name.as_os_str().is_empty() {
            // Only if not root! Avoids path spec / warning
            // and mapname conversion failed error on unzip
            zip.add_directory_from_path(name, options).unwrap();
        }
    }
    zip.finish().unwrap();

    println!("Created {}", file_path.to_str().unwrap());
}

pub mod utils {

    use std::path::Path;

    use hocg_fan_sim_assets_model::CardIllustration;
    use image::{GrayImage, RgbImage};
    use image_hasher::{HasherConfig, ImageHash};
    use imageproc::map::{blue_channel, green_channel, red_channel};

    use itertools::Itertools;
    use palette::{Hsv, IntoColor, Srgb};

    use crate::DEBUG;

    // one image can map to multiple illustrations, if they are similar enough
    pub const DIST_TOLERANCE: u64 = 3 * 3 * 3 * 2; // a distance of 3 in each color channels and 2 for saturation

    pub fn path_to_image_hash(path: &Path) -> String {
        let img = image::open(path).unwrap().into_rgb8();
        to_image_hash(&img)
    }

    pub fn to_image_hash(img: &RgbImage) -> String {
        let hasher = HasherConfig::new()
            .preproc_dct()
            .hash_alg(image_hasher::HashAlg::DoubleGradient)
            .to_hasher();

        let mut hashes = vec![];

        let red = red_channel(img);
        let green = green_channel(img);
        let blue = blue_channel(img);

        let rh = hasher.hash_image(&red);
        let gh = hasher.hash_image(&green);
        let bh = hasher.hash_image(&blue);
        hashes.push(rh.to_base64());
        hashes.push(gh.to_base64());
        hashes.push(bh.to_base64());

        let sat = sat_gray_image(img);
        let sh = hasher.hash_image(&sat);
        hashes.push(sh.to_base64());

        let hash = hashes.join("|");

        if DEBUG {
            println!("{hash}");
        }

        hash
    }

    pub fn is_similar(c1: &CardIllustration, c2: &CardIllustration) -> bool {
        let dist = dist_hash(&c1.img_hash, &c2.img_hash);
        if DEBUG {
            println!(
                "is_similar({:?}, {:?}) = {dist}",
                c1.manage_id, c2.manage_id
            );
        }
        // more tolerance for manual cropping
        dist <= DIST_TOLERANCE * 32
    }

    pub fn dist_hash(h1: &str, h2: &str) -> u64 {
        if h1.is_empty() || h2.is_empty() {
            return u64::MAX; // no hash, no distance
        }

        let h1 = h1.split('|').collect_vec();
        let h2 = h2.split('|').collect_vec();

        let a_dist_h = h1
            .iter()
            .zip(h2.iter())
            .filter_map(|(h1, h2)| {
                Some(
                    ImageHash::<Box<[u8]>>::from_base64(h1)
                        .ok()?
                        .dist(&ImageHash::from_base64(h2).ok()?) as u64,
                )
            })
            .collect_vec();
        if h1.len() != a_dist_h.len() || h2.len() != a_dist_h.len() {
            return u64::MAX;
        }
        let dist_h: u64 = a_dist_h
            .iter()
            .map(|d| if *d <= 2 { 1 } else { *d })
            .product();

        if DEBUG {
            println!("{a_dist_h:?} = {dist_h}");
        }
        dist_h
    }

    fn sat_gray_image(img: &RgbImage) -> GrayImage {
        GrayImage::from_raw(
            img.width(),
            img.height(),
            img.pixels()
                .flat_map(|p| {
                    let s = hsv(p.0).1;
                    [((1.0 - s) * 255.0) as u8]
                })
                .collect(),
        )
        .unwrap()
    }

    fn hsv(ps: [u8; 3]) -> (f32, f32, f32) {
        let rgb = Srgb::from(ps).into_format();
        let hsv: Hsv = rgb.into_color();
        let h = hsv.hue.into_positive_degrees();
        let s = hsv.saturation;
        let v = hsv.value;
        (h, s, v)
    }
}
