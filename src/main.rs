use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{atomic::AtomicU32, Arc, OnceLock},
};

use clap::Parser;
use oxipng::{InFile, Options, OutFile};
use parking_lot::{Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};
use rayon::iter::{
    IntoParallelIterator, IntoParallelRefIterator, ParallelBridge, ParallelIterator,
};
use reqwest::{
    blocking::{Client, ClientBuilder},
    header::{LAST_MODIFIED, REFERER},
};
use serde::{Deserialize, Serialize};
use tempfile::TempDir;
use walkdir::WalkDir;
use webp::{Encoder, WebPMemory};
use zip::write::SimpleFileOptions;

static WEBP_QUALITY: f32 = 80.0;

fn http_client() -> &'static Client {
    static HTTP_CLIENT: OnceLock<Client> = OnceLock::new();
    HTTP_CLIENT.get_or_init(|| ClientBuilder::new().cookie_store(true).build().unwrap())
}

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
struct CardEntry {
    manage_id: String,
    card_number: String,
    img: String,
    max: StringOrNumber,
    #[serde(default)]
    deck_type: String,
    #[serde(default)]
    img_last_modified: Option<String>,
    #[serde(default)]
    img_proxy_en: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum StringOrNumber {
    String(String),
    Number(u32),
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
}

fn main() {
    let args = Args::parse();

    let mut all_cards: BTreeMap<u32, CardEntry> = BTreeMap::new();

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

    let filtered_cards = retrieve_card_info(
        &mut all_cards,
        &args.number_filter,
        &args.expansion,
        args.optimized_original_images,
    );

    if args.download_images {
        download_images(
            &filtered_cards,
            &images_path,
            &mut all_cards,
            args.force_download,
            args.optimized_original_images,
        );
    }

    if let Some(path) = args.proxy_path {
        prepare_proxy_images(&filtered_cards, &images_proxy_path, &mut all_cards, path);
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
    all_cards: &mut BTreeMap<u32, CardEntry>,
    number_filter: &Option<String>,
    expansion: &Option<String>,
    optimized_original_images: bool,
) -> Vec<u32> {
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
                                card.max = StringOrNumber::Number(match &card.max {
                                    StringOrNumber::String(s) => {
                                        s.parse().expect("should be a number")
                                    }
                                    StringOrNumber::Number(n) => *n,
                                });

                                let key = card.manage_id.parse::<u32>().unwrap();
                                filtered_cards.lock().push(key);
                                all_cards
                                    .write()
                                    .entry(key)
                                    .and_modify({
                                        let card = card.clone();
                                        |c| {
                                            // only these fields are retrieved
                                            c.card_number = card.card_number;
                                            c.img = card.img;
                                            c.max = card.max;
                                            c.deck_type = card.deck_type;
                                        }
                                    })
                                    .or_insert(card);
                            }

                            Some(())
                        }
                    })
                    .while_some()
            }
        })
        .max(); // need this to drive the iterator

    let filtered_cards = filtered_cards.lock();
    filtered_cards.clone()
}

fn download_images(
    filtered_cards: &[u32],
    images_path: &Path,
    all_cards: &mut BTreeMap<u32, CardEntry>,
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
        move |card| {
            // https://hololive-official-cardgame.com/wp-content/images/cardlist/hSD01/hSD01-006_RR.png

            let img_last_modified;
            // scope for the read guard
            {
                let card = RwLockReadGuard::map(all_cards.read(), |ac| ac.get(card).unwrap());

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
            let mut card = RwLockWriteGuard::map(all_cards.write(), |ac| ac.get_mut(card).unwrap());
            card.img_last_modified = img_last_modified;
        }
    });

    let image_count = image_count.load(std::sync::atomic::Ordering::Relaxed);
    let image_skipped = image_skipped.load(std::sync::atomic::Ordering::Relaxed);
    println!("{image_count} images downloaded ({image_skipped} skipped)");
}

fn prepare_proxy_images(
    filtered_cards: &[u32],
    images_proxy_path: &Path,
    all_cards: &mut BTreeMap<u32, CardEntry>,
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
        if !entry.path().ancestors().any(|p| {
            p.file_name()
                .unwrap_or_default()
                .eq_ignore_ascii_case("blanks")
        }) && entry.path().is_file()
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
        move |card| {
            let img_proxy_en;
            // scope for the read guard
            {
                let card = RwLockReadGuard::map(all_cards.read(), |ac| ac.get(card).unwrap());

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
            let mut card = RwLockWriteGuard::map(all_cards.write(), |ac| ac.get_mut(card).unwrap());
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
