use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File},
    io::{Read, Write},
    path::Path,
    sync::{Arc, atomic::AtomicU32},
};

use hocg_fan_sim_assets_model::{CardIllustration, CardsDatabase};
use image::DynamicImage;
use itertools::Itertools;
use oxipng::{InFile, Options, OutFile};
use parking_lot::{Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};
use rayon::iter::{IntoParallelIterator, IntoParallelRefIterator, ParallelIterator};
use reqwest::header::{LAST_MODIFIED, REFERER};
use walkdir::WalkDir;
use webp::{Encoder, WebPMemory};
use zip::write::SimpleFileOptions;

use crate::{
    DEBUG,
    holodelta::DIST_TOLERANCE,
    http_client,
    images::utils::{dist_hash, to_image_hash},
};

static WEBP_QUALITY: f32 = 80.0;

pub fn download_images(
    filtered_cards: &[(String, usize)],
    images_jp_path: &Path,
    all_cards: &mut CardsDatabase,
    force_download: bool,
    optimized_original_images: bool,
) {
    println!("Downloading {} images...", filtered_cards.len());

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

                // check if it's a new image
                let resp = http_client()
                    .head(format!(
                        "https://hololive-official-cardgame.com/wp-content/images/cardlist/{}",
                        card.img_path
                            .japanese
                            .as_deref()
                            .unwrap_or_default()
                            .replace(".webp", ".png")
                    ))
                    .header(REFERER, "https://decklog.bushiroad.com/")
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
                    .as_deref()
                    .map(httpdate::parse_http_date);
                if let (Some(Ok(last_modified_time)), Some(Ok(card_last_modified_time))) =
                    (last_modified_time, card_last_modified_time)
                {
                    if last_modified_time <= card_last_modified_time && !force_download {
                        // we already have the image
                        image_skipped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        return;
                    }
                }

                img_last_modified = last_modified
                    .map(String::from)
                    .or(card.img_last_modified.clone());

                // download the image
                let resp = http_client()
                    .get(format!(
                        "https://hololive-official-cardgame.com/wp-content/images/cardlist/{}",
                        card.img_path
                            .japanese
                            .as_deref()
                            .unwrap_or_default()
                            .replace(".webp", ".png")
                    ))
                    .header(REFERER, "https://decklog.bushiroad.com/")
                    .send()
                    .unwrap();

                // Using `image` crate, open the included .jpg file
                let img = image::load_from_memory(&resp.bytes().unwrap()).unwrap();

                if optimized_original_images {
                    let path =
                        images_jp_path.join(card.img_path.japanese.as_deref().unwrap_or_default());
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
                    let path = images_jp_path.join(
                        card.img_path
                            .japanese
                            .as_deref()
                            .unwrap_or_default()
                            .replace(".png", ".webp"),
                    );
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
            card.img_last_modified = img_last_modified;
        }
    });

    let image_count = image_count.load(std::sync::atomic::Ordering::Relaxed);
    let image_skipped = image_skipped.load(std::sync::atomic::Ordering::Relaxed);
    println!("{image_count} images downloaded ({image_skipped} skipped)");
}

pub fn prepare_en_proxy_images(
    filtered_cards: &[(String, usize)],
    images_en_path: &Path,
    all_cards: &mut CardsDatabase,
    proxy_path: &Path,
    images_jp_path: &Path,
) {
    if !proxy_path.is_dir() {
        panic!("proxy_path should be dir");
    }

    println!("Preparing {} proxy images...", filtered_cards.len());

    // key: (number, rarity), value: (file_name, img_path)
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
        {
            if let Some(file_stem) = entry.path().file_stem() {
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

                let paths = files
                    .entry((card_number.to_owned(), rarity.to_owned()))
                    .or_default();
                if paths.iter().any(|(f, _)| *f == file_stem) {
                    if DEBUG {
                        println!("Skipping duplicate proxy image: {}", entry.path().display());
                    }
                } else {
                    // add the file name and path
                    paths.push((file_stem.to_owned(), entry.path().to_owned()));
                }
            }
        }
    }

    let proxies = Arc::new(RwLock::new(files));
    let proxies_count = AtomicU32::new(0);
    let proxies_skipped = AtomicU32::new(0);

    let rarities = all_cards
        .values_mut()
        .flat_map(|c| {
            c.illustrations
                .iter_mut()
                .enumerate()
                .filter(|(i, c)| filtered_cards.contains(&(c.card_number.clone(), *i)))
                .map(|(_, c)| c)
        })
        .map(|c| {
            // clear any existing proxy
            c.img_path.english = None;
            Arc::new(Mutex::new(c))
        })
        .into_group_map_by(|c| {
            let c = c.lock();
            (c.card_number.clone(), c.rarity.clone())
        });

    rarities
        .into_par_iter()
        .for_each(|((card_number, rarity), mut illustrations)| {
            if let Some(img_paths) = proxies
                .write()
                .get_mut(&(card_number.clone(), rarity.clone()))
            {
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
                    let (_file_name, img_path) = img_paths.swap_remove(0);
                    let illust = illustrations.swap_remove(0);
                    let mut illust = illust.lock();
                    illust.img_path.english =
                        Some(save_proxy_image(&illust, &img_path, images_en_path));
                    proxies_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    return;
                }

                // find the best match, otherwise
                println!();
                let proxies: Vec<_> = img_paths
                    .par_iter()
                    .map(|(_, img_path)| {
                        // download the image
                        println!("Checking proxy image: {}", img_path.display());
                        let proxy_img =
                            image::load_from_memory(&fs::read(img_path).unwrap()).unwrap();
                        (img_path.to_owned(), proxy_img)
                    })
                    .collect();

                let mut dists: Vec<_> = proxies
                    .into_iter()
                    .cartesian_product(illustrations.iter())
                    .map(|((img_path, proxy_img), illust)| {
                        let dist = dist_proxy_image(&illust.lock(), proxy_img, images_jp_path);

                        (img_path, illust, dist)
                    })
                    .collect();

                // sort by best dist, then update the proxy
                dists.sort_by_key(|d| d.2);

                // modify the cards here, to avoid borrowing issue
                let mut already_set = BTreeMap::new();
                for (img_path, illust, dist) in dists {
                    let mut illust = illust.lock();
                    // to handle multiple cards with the same image
                    let min_dist = *already_set
                        .get(&img_path)
                        .unwrap_or(&(u64::MAX - DIST_TOLERANCE));
                    if illust.img_path.english.is_none() && min_dist + DIST_TOLERANCE >= dist {
                        illust.img_path.english =
                            Some(save_proxy_image(&illust, &img_path, images_en_path));
                        img_paths.retain(|(_, p)| *p != img_path);
                        already_set.insert(img_path, dist.min(min_dist));
                        proxies_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                        if DEBUG {
                            println!(
                                "Updated card {:?} -> manage_id: {}, proxy path: {} ({})",
                                illust.card_number,
                                illust.manage_id.unwrap(),
                                illust.img_path.english.as_ref().unwrap(),
                                dist
                            );
                        }
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
    for ((number, rare), proxies) in proxies {
        for proxy in proxies {
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

fn dist_proxy_image(
    card: &CardIllustration,
    proxy_img: DynamicImage,
    images_jp_path: &Path,
) -> u64 {
    // compare the image to the card
    println!(
        "Checking Card image: {}",
        card.img_path.japanese.as_deref().unwrap_or_default()
    );
    let path = images_jp_path.join(card.img_path.japanese.as_deref().unwrap_or_default());
    let card_img = image::open(path).unwrap();

    let h1 = to_image_hash(&proxy_img.into_rgb8());
    let h2 = to_image_hash(&card_img.into_rgb8());

    let dist = dist_hash(&h1, &h2);

    if DEBUG {
        println!("Proxy hash: {}", h1);
        println!("Card hash: {}", h2);
        println!("Distance: {}", dist);
    }

    dist
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

    use image::{GrayImage, RgbImage};
    use image_hasher::{HasherConfig, ImageHash};
    use imageproc::map::{blue_channel, green_channel, red_channel};

    use itertools::Itertools;
    use palette::{Hsv, IntoColor, Srgb};

    use crate::DEBUG;

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
            println!("{}", hash);
        }

        hash
    }

    pub fn dist_hash(h1: &str, h2: &str) -> u64 {
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
            println!("{:?} = {}", a_dist_h, dist_h);
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
