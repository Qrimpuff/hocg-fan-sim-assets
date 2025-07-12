use std::{
    collections::{BTreeMap, HashMap},
    fs::File,
    io::BufReader,
    path::Path,
    sync::Arc,
};

use hocg_fan_sim_assets_model::{BloomLevel, CardType, CardsDatabase, Color, SupportType};
use itertools::Itertools;
use parking_lot::Mutex;
use rayon::iter::{IntoParallelIterator, IntoParallelRefMutIterator, ParallelIterator};
use serde::{Deserialize, Serialize};

use crate::{
    DEBUG,
    images::utils::{DIST_TOLERANCE, dist_hash, to_image_hash},
};

pub fn import_holodelta_db(all_cards: &mut CardsDatabase, holodelta_path: &Path) {
    println!("Importing holoDelta db images...");

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
            println!("\nProcessing card {card_number:?}");
        }

        // update holoDelta art indexes, based on card image
        if let Some(card) = all_cards.get_mut(&card_number) {
            let delta_cards: Vec<_> = delta_cards
                .1
                .into_par_iter()
                .map(|delta_card| {
                    let delta_img = image::load_from_memory(&delta_card.3).unwrap();
                    let delta_img = delta_img.into_rgb8();
                    (delta_card, delta_img)
                })
                .collect();

            let cards: Vec<_> = card
                .illustrations
                .par_iter_mut()
                .filter_map(|illust| {
                    // clear the delta art index, will be set later
                    illust.delta_art_index = None;

                    Some(Arc::new(Mutex::new(illust)))
                })
                .collect();

            let mut dists = delta_cards
                .iter()
                .cartesian_product(cards.iter())
                .map(|((delta_card, delta_img), card)| {
                    let h1 = to_image_hash(delta_img);
                    let h2 = { card.lock().img_hash.clone() };

                    let dist = dist_hash(&h1, &h2);

                    if DEBUG {
                        let card = card.lock();
                        println!("holoDelta hash: {} = {}", delta_card.1, h1);
                        println!(
                            "Card hash: {} {} = {}",
                            card.card_number,
                            card.manage_id.japanese.unwrap(),
                            h2
                        );
                        println!("Distance: {dist}");
                    }

                    (delta_card.1, card, dist)
                })
                .collect_vec();

            // sort by best dist, then update the art index
            dists.sort_by_key(|d| (d.2, d.1.lock().manage_id.japanese));

            // modify the cards here, to avoid borrowing issue
            let mut already_set = BTreeMap::new();
            for (delta_art_index, card, dist) in dists {
                // println!("dist: {:?}", (delta_art_index, card, dist));

                let mut card = card.lock();
                // to handle multiple cards with the same image
                let min_dist = *already_set
                    .get(&delta_art_index)
                    .unwrap_or(&(u64::MAX - DIST_TOLERANCE));
                if card.delta_art_index.is_none() && min_dist + DIST_TOLERANCE >= dist {
                    card.delta_art_index = Some(delta_art_index);
                    already_set.insert(delta_art_index, dist.min(min_dist));
                    updated_count += 1;

                    if DEBUG {
                        println!(
                            "Updated card {:?} -> manage_id: {}, delta_art_index: {} ({})",
                            card.card_number,
                            card.manage_id.japanese.unwrap(),
                            card.delta_art_index.unwrap(),
                            dist
                        );
                    }
                }
            }
        }
    }

    println!("Processed {total_count} holoDelta cards");
    println!("Updated {updated_count} hOCG cards");
}

pub fn import_holodelta(all_cards: &mut CardsDatabase, holodelta_path: &Path) {
    println!("Importing holoDelta repository images...");

    let card_data_path = holodelta_path.join("cardData.json");
    let file = File::open(&card_data_path).expect("Failed to open cardData.json");
    let file = BufReader::new(file);
    let delta_cards: HashMap<String, Card> = serde_json::from_reader(file).unwrap();

    let banlist_path = holodelta_path.join("ServerStuff/data_source/banlists/current.json");
    let file = File::open(&banlist_path)
        .expect("Failed to open ServerStuff/data_source/banlists/current.json");
    let file = BufReader::new(file);
    let banlist: HashMap<String, i32> = serde_json::from_reader(file).unwrap();

    // for every card number, find the best match
    let mut total_count = 0;
    let mut updated_count = 0;
    for (delta_card_number, delta_cards) in delta_cards {
        total_count += delta_cards
            .card_art
            .as_ref()
            .map(|arts| arts.len())
            .unwrap_or(0);
        let card_number = delta_card_number;

        if DEBUG {
            println!("\nProcessing card {card_number:?}");
        }

        // update holoDelta art indexes, based on card image
        if let Some(card) = all_cards.get_mut(&card_number) {
            // warn if holoDelta has different card data
            delta_cards.verify_card(&card_number, card, &banlist);

            let delta_cards: Vec<_> = delta_cards
                .card_art
                .into_par_iter()
                .flatten()
                .filter(|(_, card_art)| card_art.ja.is_some())
                .map(|(delta_art_index, _)| {
                    let (set, num) = card_number
                        .split_once('-')
                        .expect("should always have a '-'");
                    let img_path = format!(r"cardFronts\{set}\ja\{num}\{delta_art_index}.webp");
                    let file = File::open(holodelta_path.join(&img_path))
                        .map_err(|e| {
                            eprintln!("Error opening file {img_path}: {e}");
                            e
                        })
                        .unwrap();
                    let file = BufReader::new(file);
                    let delta_img = image::load(file, image::ImageFormat::WebP).unwrap();
                    let delta_img = delta_img.into_rgb8();
                    (delta_art_index, delta_img)
                })
                .collect();

            let cards: Vec<_> = card
                .illustrations
                .par_iter_mut()
                .flat_map(|illust| {
                    // clear the delta art index, will be set later
                    illust.delta_art_index = None;

                    Some(Arc::new(Mutex::new(illust)))
                })
                .collect();

            let mut dists = delta_cards
                .iter()
                .cartesian_product(cards.iter())
                .map(|((delta_art_index, delta_img), card)| {
                    let h1 = to_image_hash(delta_img);
                    let h2 = { card.lock().img_hash.clone() };

                    let dist = dist_hash(&h1, &h2);

                    if DEBUG {
                        let card = card.lock();
                        println!("holoDelta hash: {delta_art_index} = {h1}");
                        println!(
                            "Card hash: {} {} = {}",
                            card.card_number,
                            card.manage_id.japanese.unwrap(),
                            h2
                        );
                        println!("Distance: {dist}");
                    }

                    (delta_art_index, card, dist)
                })
                .collect_vec();

            // sort by best dist, then update the art index
            dists.sort_by_key(|d| (d.2, d.1.lock().manage_id.japanese));

            // modify the cards here, to avoid borrowing issue
            let mut already_set = BTreeMap::new();
            for (delta_art_index, card, dist) in dists {
                // println!("dist: {:?}", (delta_art_index, card, dist));

                let mut card = card.lock();
                // to handle multiple cards with the same image
                let min_dist = *already_set
                    .get(&delta_art_index)
                    .unwrap_or(&(u64::MAX - DIST_TOLERANCE));
                if card.delta_art_index.is_none() && min_dist + DIST_TOLERANCE >= dist {
                    card.delta_art_index = Some(delta_art_index.parse().unwrap());
                    already_set.insert(delta_art_index, dist.min(min_dist));
                    updated_count += 1;

                    if DEBUG {
                        println!(
                            "Updated card {:?} -> manage_id: {}, delta_art_index: {} ({})",
                            card.card_number,
                            card.manage_id.japanese.unwrap(),
                            card.delta_art_index.unwrap(),
                            dist
                        );
                    }
                }
            }
        }
    }

    println!("Processed {total_count} holoDelta cards");
    println!("Updated {updated_count} hOCG cards");
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Card {
    #[serde(default)]
    pub arts: Option<Vec<Art>>,
    #[serde(default, rename = "batonPassCost")]
    pub baton_pass_cost: Option<u32>,
    #[serde(default)]
    pub buzz: Option<bool>,
    #[serde(default, rename = "cardArt")]
    pub card_art: Option<HashMap<String, CardArtLangs>>,
    #[serde(default, rename = "cardLimit")]
    pub card_limit: Option<i32>,
    #[serde(default, rename = "cardType")]
    pub card_type: Option<String>,
    #[serde(default)]
    pub color: Option<CardColor>,
    #[serde(default)]
    pub effect: Option<String>,
    #[serde(default)]
    pub hp: Option<u32>,
    #[serde(default)]
    pub level: Option<i32>,
    #[serde(default)]
    pub name: Option<Vec<String>>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub skills: Option<Vec<Skill>>,
    #[serde(default, rename = "supportType")]
    pub support_type: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Art {
    #[serde(rename = "artIndex")]
    pub art_index: u32,
    pub cost: String,
    pub damage: u32,
    #[serde(rename = "hasEffect")]
    pub has_effect: bool,
    #[serde(rename = "hasPlus")]
    pub has_plus: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CardArtLangs {
    #[serde(default)]
    pub en: Option<CardArtInfo>,
    #[serde(default)]
    pub ja: Option<CardArtInfo>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CardArtInfo {
    pub proxy: bool,
    pub unrevealed: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CardColor {
    Single(String),
    Multiple(Vec<String>),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Skill {
    pub cost: i32,
    pub sp: bool,
}

impl Card {
    fn verify_card(
        &self,
        card_number: &str,
        card: &hocg_fan_sim_assets_model::Card,
        banlist: &HashMap<String, i32>,
    ) {
        // warn if the card number is different
        if card.card_number != card_number {
            eprintln!(
                "Warning: {card_number} number mismatch: {} should be {}",
                card_number, card.card_number
            );
        }

        // warn if the baton pass is different
        if card.baton_pass.len() as u32 != self.baton_pass_cost.unwrap_or_default() {
            eprintln!(
                "Warning: {card_number} baton pass mismatch: {:?} should be {:?}",
                self.baton_pass_cost,
                card.baton_pass.len()
            );
        }

        // warn if buzz is different
        if card.buzz != self.buzz.unwrap_or_default() {
            eprintln!(
                "Warning: {card_number} buzz mismatch: {:?} should be {:?}",
                self.buzz, card.buzz
            );
        }

        // warn if card limit is different
        if match card.card_type {
            CardType::OshiHoloMember => {
                card.max_amount.japanese.unwrap() as i32
                    != self.card_limit.unwrap_or_default().min(1)
            }
            CardType::Cheer => {
                card.max_amount.japanese.unwrap() as i32
                    != self
                        .card_limit
                        .map(|l| if l == -1 { 20 } else { l })
                        .unwrap_or_default()
            }
            _ => {
                card.max_amount.japanese.unwrap() as i32
                    != self
                        .card_limit
                        .map(|l| if l == -1 { 50 } else { l })
                        .unwrap_or_default()
                    && card.max_amount.japanese.unwrap() as i32
                        != banlist.get(card_number).copied().unwrap_or_default()
            }
        } {
            eprintln!(
                "Warning: {card_number} card limit mismatch: {:?} should be {:?}",
                self.card_limit, card.max_amount
            );
        }

        // warn if card type is different
        if match card.card_type {
            CardType::OshiHoloMember => (Some("Oshi".into()), None),
            CardType::HoloMember => (Some("Holomem".into()), None),
            CardType::Support(SupportType::Staff) => (Some("Support".into()), Some("Staff".into())),
            CardType::Support(SupportType::Item) => (Some("Support".into()), Some("Item".into())),
            CardType::Support(SupportType::Event) => (Some("Support".into()), Some("Event".into())),
            CardType::Support(SupportType::Tool) => (Some("Support".into()), Some("Tool".into())),
            CardType::Support(SupportType::Mascot) => {
                (Some("Support".into()), Some("Mascot".into()))
            }
            CardType::Support(SupportType::Fan) => (Some("Support".into()), Some("Fan".into())),
            CardType::Cheer => (Some("Cheer".into()), None),
            CardType::Other => (Some("Other".into()), None),
        } != (self.card_type.clone(), self.support_type.clone())
        {
            eprintln!(
                "Warning: {card_number} card type mismatch: {:?} should be {:?}",
                self.card_type, card.card_type
            );
        }

        // warn if color is different
        if card
            .colors
            .iter()
            .filter(|c| *c != &Color::Colorless)
            .map(|c| format!("{c:?}"))
            .collect_vec()
            != match &self.color {
                Some(CardColor::Single(c)) => vec![c.clone()],
                Some(CardColor::Multiple(c)) => c.clone(),
                None => vec![],
            }
        {
            eprintln!(
                "Warning: {card_number} color mismatch: {:?} should be {:?}",
                self.color, card.colors
            );
        }

        // warn if hp is different
        if card.hp != self.hp.unwrap_or_default() {
            eprintln!(
                "Warning: {card_number} hp mismatch: {:?} should be {:?}",
                self.hp, card.hp
            );
        }

        // warn if level is different
        if card.bloom_level.as_ref().map(|l| match l {
            BloomLevel::Debut => 0,
            BloomLevel::First => 1,
            BloomLevel::Second => 2,
            BloomLevel::Spot => -1,
        }) != self.level
        {
            eprintln!(
                "Warning: {card_number} level mismatch: {:?} should be {:?}",
                self.level, card.bloom_level
            );
        }

        // warn if tags is different
        if card
            .tags
            .iter()
            .map(|t| {
                t.english
                    .clone()
                    .unwrap_or_default()
                    .to_uppercase()
                    .trim_start_matches('#')
                    .to_string()
            })
            .sorted()
            .collect_vec()
            != self
                .tags
                .clone()
                .unwrap_or_default()
                .into_iter()
                .map(|t| t.to_uppercase())
                .sorted()
                .collect_vec()
        {
            // eprintln!(
            //     "Warning: {card_number} tags mismatch: {:?} should be {:?}",
            //     self.tags, card.tags
            // );
        }
    }
}
