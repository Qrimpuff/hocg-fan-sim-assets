use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub type CardsDatabase = BTreeMap<String, Card>;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CardType {
    #[serde(rename = "oshi_holomem")]
    OshiHoloMember,
    #[serde(rename = "holomem")]
    HoloMember,
    Support(SupportType),
    Cheer,
    #[default]
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportType {
    Staff,
    Item,
    Event,
    Tool,
    Mascot,
    Fan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Color {
    White,
    Green,
    Red,
    Blue,
    Purple,
    Yellow,
    Colorless,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BloomLevel {
    Debut,
    First,
    Second,
    Spot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
#[serde(default)]
pub struct Card {
    pub card_number: String,
    pub name: Localized<String>,
    pub card_type: CardType,
    #[serde(skip_serializing_if = "is_default")]
    pub colors: Vec<Color>, // oshi, holomem
    #[serde(skip_serializing_if = "is_default")]
    pub life: u32, // oshi
    #[serde(skip_serializing_if = "is_default")]
    pub hp: u32, // holomem
    #[serde(skip_serializing_if = "is_default")]
    pub bloom_level: Option<BloomLevel>, // holomem
    #[serde(skip_serializing_if = "is_default")]
    pub buzz: bool, // holomem
    #[serde(skip_serializing_if = "is_default")]
    pub limited: bool, // support
    pub text: Localized<String>,
    #[serde(skip_serializing_if = "is_default")]
    pub tags: Vec<Localized<String>>, // holomem, support
    #[serde(skip_serializing_if = "is_default")]
    pub baton_pass: Vec<Color>, // holomem
    pub max_amount: u32,
    pub illustrations: Vec<CardIllustration>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct CardIllustration {
    pub card_number: String,
    pub manage_id: Option<u32>, // unique id in Deck Log
    pub rarity: String,
    pub img_path: Localized<String>,
    #[serde(skip_serializing_if = "is_default")]
    pub img_last_modified: Option<String>,
    pub yuyutei_sell_url: Option<String>,
    pub delta_art_index: Option<u32>,
}

fn is_default<T: Default + Eq>(value: &T) -> bool {
    value == &T::default()
}
