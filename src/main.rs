use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    sync::{atomic::AtomicU32, Arc, Mutex, OnceLock},
};

use clap::Parser;
use rayon::iter::{
    IntoParallelIterator, IntoParallelRefMutIterator, ParallelBridge, ParallelIterator,
};
use reqwest::{
    blocking::{Client, ClientBuilder},
    header::{LAST_MODIFIED, REFERER},
};
use serde::{Deserialize, Serialize};
use webp::{Encoder, WebPMemory};

static CARD_MAPPING_FILE: &str = "assets/cards_info.json";
static IMAGES_PATH: &str = "assets/img";

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

    /// Download card images as webp
    #[arg(short = 'i', long)]
    download_images: bool,
}

fn main() {
    let args = Args::parse();

    let mut cards = retrieve_card_info(&args);
    merge_card_info(&mut cards);

    if args.download_images {
        download_images(&mut cards);
        merge_card_info(&mut cards);
    }

    println!("done");
}

fn retrieve_card_info(args: &Args) -> Vec<CardEntry> {
    if args.number_filter.is_none() && args.expansion.is_none() {
        println!("Retrieve ALL cards info");
    } else {
        println!(
            "Retrieve cards info with filters - number: {}, expension: {}",
            args.number_filter.as_deref().unwrap_or("all"),
            args.expansion.as_deref().unwrap_or("all")
        );
    }

    let all_cards = Arc::new(Mutex::new(Vec::new()));

    let _ = ["N", "OSHI", "YELL"]
        .into_par_iter()
        .flat_map({
            let all_cards = all_cards.clone();
            move |deck_type| {
                (1..)
                    .par_bridge()
                    .map({
                        let all_cards = all_cards.clone();
                        move |page| {
                            println!("deck type: {deck_type}, page: {page}");

                            let req = ApiSearchRequest {
                                param: ApiSearchParam {
                                    deck_param1: "S".into(),
                                    deck_type: deck_type.into(),
                                    keyword: args.number_filter.clone().unwrap_or_default(),
                                    keyword_type: vec!["no".into()],
                                    expansion: args.expansion.clone().unwrap_or_default(),
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
                            let Ok(mut cards): Result<Vec<CardEntry>, _> = cards else {
                                eprintln!("didn't like response: {content}");
                                panic!("{:?}", cards)
                            };

                            // no more card in this page
                            if cards.is_empty() {
                                return None;
                            }

                            // update records with deck type and webp images
                            for card in &mut cards {
                                card.deck_type = deck_type.into();
                                card.img = card.img.replace(".png", ".webp");
                                card.max = StringOrNumber::Number(match &card.max {
                                    StringOrNumber::String(s) => {
                                        s.parse().expect("should be a number")
                                    }
                                    StringOrNumber::Number(n) => *n,
                                });
                            }

                            all_cards.lock().unwrap().extend(cards);
                            Some(())
                        }
                    })
                    .while_some()
            }
        })
        .max(); // need this to drive the iterator

    let all_cards = all_cards.lock().unwrap();
    all_cards.clone()
}

fn merge_card_info(cards: &mut [CardEntry]) {
    let mut all_cards: BTreeMap<u32, CardEntry> = BTreeMap::new();

    if let Ok(s) = fs::read_to_string(CARD_MAPPING_FILE) {
        all_cards = serde_json::from_str(&s).unwrap();
    }

    for card in cards {
        all_cards
            .entry(card.manage_id.parse::<u32>().unwrap())
            .and_modify(|c| {
                // last modified is for images. updates before checking images
                if let img_last_modified @ None = &mut card.img_last_modified {
                    *img_last_modified = c.img_last_modified.clone();
                }
                *c = card.clone();
            })
            .or_insert(card.clone());
    }

    if let Some(parent) = Path::new(CARD_MAPPING_FILE).parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let json = serde_json::to_string_pretty(&all_cards).unwrap();
    fs::write(CARD_MAPPING_FILE, json).unwrap();
}

fn download_images(cards: &mut [CardEntry]) {
    println!("Downloading {} images...", cards.len());

    let image_count = AtomicU32::new(0);
    let image_skipped = AtomicU32::new(0);

    cards.par_iter_mut().for_each(|card| {
        // https://hololive-official-cardgame.com/wp-content/images/cardlist/hSD01/hSD01-006_RR.png

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
            if last_modified_time <= card_last_modified_time {
                // we already have the image
                image_skipped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                return;
            }
        }

        card.img_last_modified = last_modified
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

        // Create the WebP encoder for the above image
        let encoder: Encoder = Encoder::from_image(&img).unwrap();
        // Encode the image at a specified quality 0-100
        let webp: WebPMemory = encoder.encode(80.0);
        // Define and write the WebP-encoded file to a given path
        let path = format!("{IMAGES_PATH}/{}", card.img);
        if let Some(parent) = Path::new(&path).parent() {
            fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, &*webp).unwrap();

        image_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let image_count = image_count.load(std::sync::atomic::Ordering::Relaxed);
        let image_skipped = image_skipped.load(std::sync::atomic::Ordering::Relaxed);
        if image_count % 10 == 0 {
            println!("{image_count} images downloaded ({image_skipped} skipped)");
        }
    });

    let image_count = image_count.load(std::sync::atomic::Ordering::Relaxed);
    let image_skipped = image_skipped.load(std::sync::atomic::Ordering::Relaxed);
    println!("{image_count} images downloaded ({image_skipped} skipped)");
}
