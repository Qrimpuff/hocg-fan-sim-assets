pub mod data;
pub mod holodelta;
pub mod images;
pub mod ogbajoj;
pub mod price_check;
pub mod qna;
pub mod utils;

use std::sync::OnceLock;

use clap::ValueEnum;
use reqwest::blocking::{Client, ClientBuilder};

pub const DEBUG: bool = false;

fn http_client() -> &'static Client {
    static HTTP_CLIENT: OnceLock<Client> = OnceLock::new();
    HTTP_CLIENT.get_or_init(|| ClientBuilder::new().cookie_store(true).build().unwrap())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Language {
    All,
    Japanese,
    English,
}

impl From<Language> for hocg_fan_sim_assets_model::Language {
    fn from(value: Language) -> Self {
        match value {
            Language::All => panic!(
                "Language::All is not a valid language for hocg_fan_sim_assets_model::Language"
            ),
            Language::Japanese => hocg_fan_sim_assets_model::Language::Japanese,
            Language::English => hocg_fan_sim_assets_model::Language::English,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PriceCheckMode {
    /// Use the first urls found
    Quick,
    /// Compare images to find the best match
    Images,
}
