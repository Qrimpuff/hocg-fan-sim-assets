use std::{collections::BTreeMap, fmt::Display, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize};

// need to keep the order to know which card image to use
// (holoDelta is using a zero-based index)
pub type CardsInfo = BTreeMap<String, Vec<CardEntry>>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CardEntry {
    pub card_number: String, // from Deck Log
    #[serde(deserialize_with = "deserialize_nullable_number_from_string")]
    pub manage_id: Option<u32>, // from Deck Log
    pub rare: String,        // from Deck Log
    pub img: String,         // from Deck Log
    #[serde(deserialize_with = "deserialize_number_from_string")]
    pub max: u32, // from Deck Log
    #[serde(default)]
    pub deck_type: String, // from Deck Log
    #[serde(default)]
    pub img_last_modified: Option<String>,
    #[serde(default)]
    pub img_proxy_en: Option<String>,
    #[serde(default)]
    pub yuyutei_sell_url: Option<String>,
    #[serde(default)]
    pub delta_art_index: Option<u32>,
}

fn deserialize_number_from_string<'de, T, D>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: FromStr + serde::Deserialize<'de>,
    <T as FromStr>::Err: Display,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrInt<T> {
        String(String),
        Number(T),
    }

    match StringOrInt::<T>::deserialize(deserializer)? {
        StringOrInt::String(s) => s.parse::<T>().map_err(serde::de::Error::custom),
        StringOrInt::Number(i) => Ok(i),
    }
}

fn deserialize_nullable_number_from_string<'de, T, D>(
    deserializer: D,
) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: FromStr + serde::Deserialize<'de>,
    <T as FromStr>::Err: Display,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrInt<T> {
        String(String),
        Number(T),
    }

    match Option::<StringOrInt<T>>::deserialize(deserializer)? {
        Some(StringOrInt::String(s)) => s
            .parse::<T>()
            .map_err(serde::de::Error::custom)
            .map(Option::Some),
        Some(StringOrInt::Number(i)) => Ok(Some(i)),
        None => Ok(None),
    }
}
