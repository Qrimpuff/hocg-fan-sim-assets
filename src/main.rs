use std::{collections::BTreeMap, fs, path::Path, sync::OnceLock};

use clap::Parser;
use reqwest::{
    blocking::{Client, ClientBuilder},
    header::REFERER,
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
    param: ApiSearchParam,
    page: u32,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct ApiSearchParam {
    deck_type: String,
    keyword: String,
    keyword_type: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
struct CardEntry {
    manage_id: String,
    card_number: String,
    img: String,
    max: u32,
    #[serde(default)]
    deck_type: String,
}

/// Scrap hOCG information from Deck Log
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// The card number to retrieve e.g. hSD01-001, hBP01 (default to all)
    #[arg(short, long)]
    filter: Option<String>,

    /// Number of times to greet
    #[arg(short, long)]
    download_images: bool,
}

fn main() {
    let args = Args::parse();

    let cards = retrieve_card_info(&args);
    merge_card_info(&cards);

    if args.download_images {
        download_images(&cards);
    }

    println!("done");
}

fn retrieve_card_info(args: &Args) -> Vec<CardEntry> {
    if let Some(filter_number) = &args.filter {
        println!("Retrieve cards info for filter: {filter_number}");
    } else {
        println!("Retrieve ALL cards info");
    }

    let mut all_cards = Vec::new();

    for deck_type in ["N", "OSHI", "YELL"] {
        for page in 1.. {
            println!("deck type: {deck_type}, page: {page}");

            let req = ApiSearchRequest {
                param: ApiSearchParam {
                    deck_type: deck_type.into(),
                    keyword: args.filter.clone().unwrap_or_default(),
                    keyword_type: vec!["no".into()],
                },
                page,
            };

            let resp = http_client()
                .post("https://decklog-en.bushiroad.com/system/app-ja/api/search/108")
                .header(REFERER, "https://decklog-en.bushiroad.com/")
                .json(&req)
                .send()
                .unwrap();

            let content = resp.text().unwrap();
            let cards = serde_json::from_str(&content);
            let Ok(mut cards): Result<Vec<CardEntry>, _> = cards else {
                eprintln!("didn't like response: {content}");
                panic!("{:?}", cards)
            };

            // no more card in this page
            if cards.is_empty() {
                break;
            }

            // update records with deck type and webp images
            for card in &mut cards {
                card.deck_type = deck_type.into();
                card.img = card.img.replace(".png", ".webp");
            }

            all_cards.extend(cards);
        }
    }

    all_cards
}

fn merge_card_info(cards: &[CardEntry]) {
    let mut all_cards = BTreeMap::new();

    if let Ok(s) = fs::read_to_string(CARD_MAPPING_FILE) {
        all_cards = serde_json::from_str(&s).unwrap();
    }

    all_cards.extend(
        cards
            .iter()
            .cloned()
            .map(|e| (e.manage_id.parse::<u32>().unwrap(), e)),
    );

    if let Some(parent) = Path::new(CARD_MAPPING_FILE).parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let json = serde_json::to_string_pretty(&all_cards).unwrap();
    fs::write(CARD_MAPPING_FILE, json).unwrap();
}

fn download_images(cards: &[CardEntry]) {
    println!("Downloading {} images...", cards.len());

    let mut image_count = 0;

    for card in cards {
        image_count += 1;
        if image_count % 10 == 0 {
            println!("{image_count} images downloaded");
        }

        // https://hololive-official-cardgame.com/wp-content/images/cardlist/hSD01/hSD01-006_RR.png

        let resp = http_client()
            .get(format!(
                "https://hololive-official-cardgame.com/wp-content/images/cardlist/{}",
                card.img.replace(".webp", ".png")
            ))
            .header(REFERER, "https://decklog-en.bushiroad.com/")
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
    }

    println!("{image_count} images downloaded");
}
