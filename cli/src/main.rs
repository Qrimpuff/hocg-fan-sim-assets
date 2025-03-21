use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File},
    io::{BufReader, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, OnceLock, atomic::AtomicU32},
    time::Duration,
};

use clap::Parser;
use hocg_fan_sim_assets_model::{CardEntry, CardsInfo};
use image::{
    GrayImage,
    imageops::{crop, resize},
};
use imageproc::map::{blue_channel, green_channel, red_channel};
use indexmap::IndexMap;
use itertools::Itertools;
use oxipng::{InFile, Options, OutFile};
use parking_lot::{Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};
use rayon::iter::{
    IntoParallelIterator, IntoParallelRefIterator, ParallelBridge, ParallelIterator,
};
use reqwest::{
    Url,
    blocking::{Client, ClientBuilder},
    header::{LAST_MODIFIED, REFERER},
};
use scraper::{Html, Selector};
use serde::Serialize;
use tempfile::TempDir;
use walkdir::WalkDir;
use webp::{Encoder, WebPMemory};
use zip::write::SimpleFileOptions;

static WEBP_QUALITY: f32 = 80.0;

fn http_client() -> &'static Client {
    static HTTP_CLIENT: OnceLock<Client> = OnceLock::new();
    HTTP_CLIENT.get_or_init(|| ClientBuilder::new().cookie_store(true).build().unwrap())
}

/// Scrap hOCG information from Deck Log
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// The card number to retrieve e.g. hSD01-001 (default to all)
    #[arg(short = 'n', long)]
    number_filter: Option<String>,

    /// The expansion to retrieve e.g. hSD01, hBP01, hPR, hYS01 (default to all)
    #[arg(short = 'x', long)]
    expansion: Option<String>,

    /// Download card images as WebP
    #[arg(short = 'i', long)]
    download_images: bool,

    /// Always download card images as WebP
    #[arg(short = 'f', long)]
    force_download: bool,

    /// Download the original PNG images instead of converting to WebP
    #[arg(short = 'o', long)]
    optimized_original_images: bool,

    /// Package the image into a zip file
    #[arg(short = 'z', long)]
    zip_images: bool,

    /// Don't read existing file
    #[arg(short = 'c', long)]
    clean: bool,

    /// The path to the english proxy folder
    #[arg(short = 'p', long)]
    proxy_path: Option<PathBuf>,

    /// The folder that contains the assets i.e. card info, images, proxies
    #[arg(long, default_value = "assets")]
    assets_path: PathBuf,

    /// Don't update the cards info
    #[arg(long)]
    skip_update: bool,

    /// Update the yuyu-tei.jp urls for the cards. can only be use when all cards are searched
    #[arg(long)]
    yuyutei_urls: bool,

    /// Use holoDelta to import missing/unreleased cards data
    #[arg(long)]
    import_holodelta: bool,

    /// The file that contains the card database for holoDelta
    #[arg(long, default_value = "./cardData.db")]
    holodelta_path: PathBuf,
}

fn main() {
    let args = Args::parse();

    let mut all_cards: CardsInfo = CardsInfo::new();

    // create a temporary folder for the zip file content
    let temp = args.zip_images.then_some(TempDir::new().unwrap());
    let assets_path = if let Some(temp) = &temp {
        temp.path()
    } else {
        args.assets_path.as_path()
    };

    let card_mapping_file = assets_path.join("cards_info.json");
    let images_path = assets_path.join("img");
    let images_proxy_path = assets_path.join("img_proxy_en");

    // load file
    if !args.clean {
        if let Ok(s) = fs::read_to_string(&card_mapping_file) {
            all_cards = serde_json::from_str(&s).unwrap();
        }
    }

    let filtered_cards: Vec<(String, usize)> = if args.skip_update {
        all_cards
            .values()
            .flat_map(|cs| cs.iter().enumerate())
            // don't include unreleased cards
            .filter(|c| c.1.manage_id.is_some())
            .map(|c| (c.1.card_number.clone(), c.0))
            .collect()
    } else {
        // import cards info from Deck Log
        retrieve_card_info(
            &mut all_cards,
            &args.number_filter,
            &args.expansion,
            args.optimized_original_images,
        )
    };

    // add official images
    if args.download_images {
        download_images(
            &filtered_cards,
            &images_path,
            &mut all_cards,
            args.force_download,
            args.optimized_original_images,
        );
    }

    // add proxy images
    if let Some(path) = args.proxy_path {
        prepare_proxy_images(&filtered_cards, &images_proxy_path, &mut all_cards, path);
    }

    // update yuyutei price
    if args.yuyutei_urls {
        if args.number_filter.is_some() || args.expansion.is_some() {
            eprintln!("WARNING: SKIPPING YUYUTEI. ONLY AVAILABLE WHEN SEARCHING ALL CARDS.");
        } else {
            yuyutei(&mut all_cards);
        }
    }

    // import from holoDelta
    if args.import_holodelta {
        import_holodelta(&mut all_cards, &images_path, &args.holodelta_path);
    }

    // save file
    if let Some(parent) = Path::new(&card_mapping_file).parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let json = serde_json::to_string_pretty(&all_cards).unwrap();
    fs::write(card_mapping_file, json).unwrap();

    if args.zip_images {
        zip_images(
            &format!(
                "{}-images",
                args.expansion.as_deref().unwrap_or("hocg").to_lowercase()
            ),
            &args.assets_path,
            &images_path,
        );
    }

    println!("done");
}

fn retrieve_card_info(
    all_cards: &mut CardsInfo,
    number_filter: &Option<String>,
    expansion: &Option<String>,
    optimized_original_images: bool,
) -> Vec<(String, usize)> {
    if number_filter.is_none() && expansion.is_none() {
        println!("Retrieve ALL cards info");
    } else {
        println!(
            "Retrieve cards info with filters - number: {}, expension: {}",
            number_filter.as_deref().unwrap_or("all"),
            expansion.as_deref().unwrap_or("all")
        );
    }

    let filtered_cards = Arc::new(Mutex::new(Vec::new()));
    let all_cards = Arc::new(RwLock::new(all_cards));

    let _ = ["N", "OSHI", "YELL"]
        .into_par_iter()
        .flat_map({
            let filtered_cards = filtered_cards.clone();
            let all_cards = all_cards.clone();
            move |deck_type| {
                (1..)
                    .par_bridge()
                    .map({
                        let filtered_cards = filtered_cards.clone();
                        let all_cards = all_cards.clone();
                        move |page| {
                            println!("deck type: {deck_type}, page: {page}");

                            #[derive(Debug, Serialize)]
                            #[serde(rename_all = "snake_case")]
                            struct ApiSearchRequest {
                                page: u32,
                                param: ApiSearchParam,
                            }
                            #[derive(Debug, Serialize)]
                            #[serde(rename_all = "snake_case")]
                            struct ApiSearchParam {
                                deck_param1: String,
                                deck_type: String,
                                keyword: String,
                                keyword_type: Vec<String>,
                                expansion: String,
                            }

                            let req = ApiSearchRequest {
                                param: ApiSearchParam {
                                    deck_param1: "S".into(),
                                    deck_type: deck_type.into(),
                                    keyword: number_filter.clone().unwrap_or_default(),
                                    keyword_type: vec!["no".into()],
                                    expansion: expansion.clone().unwrap_or_default(),
                                },
                                page,
                            };

                            let resp = http_client()
                                .post("https://decklog.bushiroad.com/system/app/api/search/9")
                                .header(REFERER, "https://decklog.bushiroad.com/")
                                .json(&req)
                                .send()
                                .unwrap();

                            let content = resp.text().unwrap();
                            // println!("{content}");
                            let cards = serde_json::from_str(&content);
                            let Ok(cards): Result<Vec<CardEntry>, _> = cards else {
                                eprintln!("didn't like response: {content}");
                                panic!("{:?}", cards)
                            };

                            // no more card in this page
                            if cards.is_empty() {
                                return None;
                            }

                            // update records with deck type and webp images
                            for mut card in cards {
                                card.deck_type = deck_type.into();
                                if !optimized_original_images {
                                    card.img = card.img.replace(".png", ".webp");
                                }

                                // remove the old manage_id if it exists
                                all_cards
                                    .write()
                                    .values_mut()
                                    .flatten()
                                    .filter(|c| {
                                        c.manage_id == card.manage_id
                                            && c.card_number != card.card_number
                                    })
                                    .for_each(|c| c.manage_id = None);

                                // add the card the list
                                let mut all_cards = all_cards.write();
                                let list = all_cards.entry(card.card_number.clone()).or_default();
                                // find the card, first by manage_id, then by image, then overwrite delta, otherwise just add
                                if let Some(c) = {
                                    if let Some(c) =
                                        list.iter_mut().find(|c| c.manage_id == card.manage_id)
                                    {
                                        Some(c)
                                    } else if let Some(c) =
                                        list.iter_mut().find(|c| c.img == card.img)
                                    {
                                        Some(c)
                                    } else {
                                        list.iter_mut().find(|c| c.manage_id.is_none())
                                    }
                                } {
                                    // only these fields are retrieved
                                    c.card_number = card.card_number;
                                    c.manage_id = card.manage_id;
                                    c.rare = card.rare;
                                    c.img = card.img;
                                    c.max = card.max;
                                    c.deck_type = card.deck_type;
                                } else {
                                    list.push(card.clone());
                                }

                                // sort the list, by oldest to latest
                                list.sort_by_key(|c| c.manage_id);

                                // add to filtered cards
                                filtered_cards.lock().push(card.manage_id);
                            }

                            Some(())
                        }
                    })
                    .while_some()
            }
        })
        .max(); // need this to drive the iterator

    let all_cards = all_cards.read();
    let filtered_cards = filtered_cards.lock();
    all_cards
        .values()
        .flat_map(|cs| cs.iter().enumerate())
        .filter(|c| filtered_cards.contains(&c.1.manage_id))
        .map(|c| (c.1.card_number.clone(), c.0))
        .collect()
}

fn download_images(
    filtered_cards: &[(String, usize)],
    images_path: &Path,
    all_cards: &mut CardsInfo,
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
        move |(card_number, card_idx)| {
            // https://hololive-official-cardgame.com/wp-content/images/cardlist/hSD01/hSD01-006_RR.png

            let img_last_modified;
            // scope for the read guard
            {
                let card = RwLockReadGuard::map(all_cards.read(), |ac| {
                    ac.get(card_number).unwrap().get(*card_idx).unwrap()
                });

                // check if it's a new image
                let resp = http_client()
                    .head(format!(
                        "https://hololive-official-cardgame.com/wp-content/images/cardlist/{}",
                        card.img.replace(".webp", ".png")
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
                        card.img.replace(".webp", ".png")
                    ))
                    .header(REFERER, "https://decklog.bushiroad.com/")
                    .send()
                    .unwrap();

                // Using `image` crate, open the included .jpg file
                let img = image::load_from_memory(&resp.bytes().unwrap()).unwrap();

                if optimized_original_images {
                    let path = images_path.join(&card.img);
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
                    let path = images_path.join(card.img.replace(".png", ".webp"));
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
                ac.get_mut(card_number).unwrap().get_mut(*card_idx).unwrap()
            });
            card.img_last_modified = img_last_modified;
        }
    });

    let image_count = image_count.load(std::sync::atomic::Ordering::Relaxed);
    let image_skipped = image_skipped.load(std::sync::atomic::Ordering::Relaxed);
    println!("{image_count} images downloaded ({image_skipped} skipped)");
}

fn prepare_proxy_images(
    filtered_cards: &[(String, usize)],
    images_proxy_path: &Path,
    all_cards: &mut CardsInfo,
    proxy_path: PathBuf,
) {
    if !proxy_path.is_dir() {
        panic!("proxy_path should be dir");
    }

    println!("Preparing {} proxy images...", filtered_cards.len());

    let all_cards = Arc::new(RwLock::new(all_cards));
    let image_count = AtomicU32::new(0);
    let image_skipped = AtomicU32::new(0);

    let mut map = HashMap::new();

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
        {
            if let Some(file_stem) = entry.path().file_stem() {
                map.entry(file_stem.to_owned())
                    .or_insert(entry.path().to_owned());
            }
        }
    }

    filtered_cards.par_iter().for_each({
        let all_cards = all_cards.clone();
        let image_count = &image_count;
        let image_skipped = &image_skipped;
        move |(card_number, card_idx)| {
            let img_proxy_en;
            // scope for the read guard
            {
                let card = RwLockReadGuard::map(all_cards.read(), |ac| {
                    ac.get(card_number).unwrap().get(*card_idx).unwrap()
                });

                let Some(path) = map.get(Path::new(&card.img).file_stem().unwrap_or_default())
                else {
                    image_skipped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    return;
                };

                // Using `image` crate, open the included .jpg file
                let img = image::load_from_memory(&fs::read(path).unwrap()).unwrap();

                // Create the WebP encoder for the above image
                let encoder: Encoder = Encoder::from_image(&img).unwrap();
                // Encode the image at a specified quality 0-100
                let webp: WebPMemory = encoder.encode(WEBP_QUALITY);
                // Define and write the WebP-encoded file to a given path
                let path = images_proxy_path.join(card.img.replace(".png", ".webp"));
                if let Some(parent) = Path::new(&path).parent() {
                    fs::create_dir_all(parent).unwrap();
                }
                std::fs::write(&path, &*webp).unwrap();

                img_proxy_en = Some(card.img.replace(".png", ".webp"));

                image_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let image_count = image_count.load(std::sync::atomic::Ordering::Relaxed);
                let image_skipped = image_skipped.load(std::sync::atomic::Ordering::Relaxed);
                if image_count % 10 == 0 {
                    println!("{image_count} images copied ({image_skipped} skipped)");
                }
            }

            // scope for write guard
            let mut card = RwLockWriteGuard::map(all_cards.write(), |ac| {
                ac.get_mut(card_number).unwrap().get_mut(*card_idx).unwrap()
            });
            card.img_proxy_en = img_proxy_en;
        }
    });

    let image_count = image_count.load(std::sync::atomic::Ordering::Relaxed);
    let image_skipped = image_skipped.load(std::sync::atomic::Ordering::Relaxed);
    println!("{image_count} images copied ({image_skipped} not found)");
}

fn zip_images(file_name: &str, assets_path: &Path, images_path: &Path) {
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

fn yuyutei(all_cards: &mut CardsInfo) {
    let mut urls = IndexMap::new();

    let scraperapi_key = std::env::var("SCRAPERAPI_API_KEY").ok();
    if scraperapi_key.is_some() {
        println!("using scraperapi.com");
    }

    // handle multiple pages (one page is 600 cards)
    // could be slow when there are multiple pages
    let mut page = 1;
    let mut max_page = 1;
    while page <= max_page {
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
        let url_select = Selector::parse("a").unwrap();
        let max_page_select = Selector::parse(".pagination li:nth-last-child(2) a").unwrap();

        for card_list in document.select(&card_lists) {
            let rarity: String = card_list
                .select(&rarity_select)
                .next()
                .unwrap()
                .text()
                .collect();
            for card in card_list.select(&cards_select) {
                let number: String = card.select(&number_select).next().unwrap().text().collect();
                let url = card.select(&url_select).next().unwrap().attr("href");
                if let Some(url) = url {
                    // group them by url
                    urls.entry(url.to_owned())
                        .or_insert((number, rarity.clone()));
                }
            }
        }

        if let Some(max) = document.select(&max_page_select).next() {
            max_page = max.text().collect::<String>().parse().unwrap();
            // println!("price_check: max_page: {max_page}");
        }

        page += 1;
    }
    println!("Found {} Yuyutei urls...", urls.len());

    let mut url_count = 0;
    let mut url_skipped = 0;

    // println!("BEFORE: {urls:#?}");
    // remove existing urls
    let mut existing_urls: HashMap<String, String> = HashMap::new();
    for card in all_cards
        .values_mut()
        .flatten()
        .filter(|c| c.yuyutei_sell_url.is_some())
    {
        if let Some(yuyutei_sell_url) = &card.yuyutei_sell_url {
            if urls.shift_remove(yuyutei_sell_url).is_some() {
                url_skipped += 1;
            }
            // group by image, some entries are duplicated, like hSD01-016
            existing_urls
                .entry(card.img.clone())
                .or_insert(yuyutei_sell_url.clone());
        }
    }

    // swap keys and values
    let mut urls: HashMap<_, Vec<_>> = urls.into_iter().fold(HashMap::new(), |mut map, (k, v)| {
        map.entry(v).or_default().push(k);
        map
    });

    // println!("BETWEEN: {urls:#?}");
    // add the remaining urls
    for card in all_cards
        .values_mut()
        .flatten()
        .filter(|c| c.yuyutei_sell_url.is_none())
    {
        // look some same image first
        if let Some(yuyutei_sell_url) = existing_urls.get(&card.img) {
            card.yuyutei_sell_url = Some(yuyutei_sell_url.clone());
        } else if let Some(urls) = urls.get_mut(&(card.card_number.clone(), card.rare.clone())) {
            if !urls.is_empty() {
                // take the first url (should be in chronological order, with some exceptions)
                let yuyutei_sell_url = urls.remove(0);
                card.yuyutei_sell_url = Some(yuyutei_sell_url.clone());
                // group by image, some entries are duplicated
                existing_urls
                    .entry(card.img.clone())
                    .or_insert(yuyutei_sell_url);
                url_count += 1;
            }
        }
    }

    // remove empty urls
    urls.retain(|_, urls| !urls.is_empty());
    // println!("AFTER: {urls:#?}");

    println!("{url_count} Yuyutei urls updated ({url_skipped} skipped)");
    for ((number, rare), urls) in urls {
        for url in urls {
            println!("MISSING: [{number}, {rare}] - {url}");
        }
    }
}

fn import_holodelta(all_cards: &mut CardsInfo, images_path: &Path, holodelta_path: &Path) {
    const DEBUG: bool = false;

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
                .into_par_iter()
                .map(|card| {
                    let path = images_path.join(&card.img);
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
                    card.delta_art_index = None;

                    (
                        Arc::new(Mutex::new(card)),
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
