use std::{collections::BTreeMap, fmt::Display, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize};

pub type CardsInfoMap = BTreeMap<u32, CardEntry>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CardEntry {
    pub manage_id: String,
    pub card_number: String,
    pub rare: String,
    pub img: String,
    #[serde(deserialize_with = "deserialize_number_from_string")]
    pub max: u32,
    #[serde(default)]
    pub deck_type: String,
    #[serde(default)]
    pub img_last_modified: Option<String>,
    #[serde(default)]
    pub img_proxy_en: Option<String>,
    #[serde(default)]
    pub yuyutei_sell_url: Option<String>,
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
