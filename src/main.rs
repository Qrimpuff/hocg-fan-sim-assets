use std::{collections::BTreeMap, fs, path::Path, sync::OnceLock};

use reqwest::{
    blocking::{Client, ClientBuilder},
    header::REFERER,
};
use serde::{Deserialize, Serialize};

static CARD_MAPPING_PATH: &str = "assets/card_mapping.json";

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

fn main() {
    let set = "hYS01";

    let mut all_cards = BTreeMap::new();

    if let Ok(s) = fs::read_to_string(CARD_MAPPING_PATH) {
        all_cards = serde_json::from_str(&s).unwrap();
    }

    for deck_type in ["N", "OSHI", "YELL"] {
        for page in 1.. {
            println!("deck type: {deck_type}, page: {page}");

            let req = ApiSearchRequest {
                param: ApiSearchParam {
                    deck_type: deck_type.into(),
                    keyword: set.into(),
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

            all_cards.extend(
                cards
                    .into_iter()
                    .map(|e| (e.manage_id.parse::<u32>().unwrap(), e)),
            );
        }
    }

    if let Some(parent) = Path::new(CARD_MAPPING_PATH).parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let json = serde_json::to_string_pretty(&all_cards).unwrap();
    fs::write(CARD_MAPPING_PATH, json).unwrap();
}
