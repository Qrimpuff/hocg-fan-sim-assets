use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub type CardsInfo2 = BTreeMap<String, Card>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Localized<T> {
    #[serde(rename = "jp")]
    pub japanese: T,
    #[serde(rename = "en")]
    pub english: Option<T>,
}

impl<T> Localized<T> {
    pub fn new(jp: T, en: T) -> Self {
        Self {
            japanese: jp,
            english: Some(en),
        }
    }

    pub fn jp(jp: T) -> Self {
        Self {
            japanese: jp,
            english: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CardType {
    OshiHolomem,
    Holomem,
    Support(SupportType),
    Cheer,
    #[default]
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportType {
    Staff,
    Item,
    Event,
    Tool,
    Mascot,
    Fan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Color {
    White,
    Green,
    Red,
    Blue,
    Purple,
    Yellow,
    ColorLess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BloomLevel {
    Debut,
    First,
    Second,
    Spot,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct Card {
    pub card_number: String,
    pub name: Localized<String>,
    pub card_type: CardType,
    pub colors: Vec<Color>,              // oshi, holomem
    pub life: u32,                       // oshi
    pub hp: u32,                         // holomem
    pub bloom_level: Option<BloomLevel>, // holomem
    pub buzz: bool,                      // holomem
    pub limited: bool,                   // support
    pub text: Localized<String>,         // TODO maybe split text into arts/effects
    pub tags: Vec<Localized<String>>,    // holomem, support
    pub baton_pass: Vec<Color>,          // holomem
    pub max_amount: u32,
    pub illustrations: Vec<CardIllustration>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct CardIllustration {
    pub card_number: String,
    pub manage_id: Option<u32>, // unique id in Deck Log
    pub rarity: String,
    pub img_path: Localized<String>,
    pub img_last_modified: Option<String>,
    pub yuyutei_sell_url: Option<String>,
    pub delta_art_index: Option<u32>,
}
