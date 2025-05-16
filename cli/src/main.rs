mod data;
mod holodelta;
mod images;
mod price_check;

use std::{
    fs::{self},
    path::{Path, PathBuf},
    sync::OnceLock,
};

use clap::Parser;
use clap::ValueEnum;
use data::{
    decklog::retrieve_card_info_from_decklog, hololive_official::retrieve_card_info_from_hololive,
    ogbajoj::retrieve_card_info_from_ogbajoj_sheet,
};
use hocg_fan_sim_assets_model::CardsDatabase;
use holodelta::{import_holodelta, import_holodelta_db};
use images::{download_images, prepare_en_proxy_images, zip_images};
use json_pretty_compact::PrettyCompactFormatter;
use price_check::yuyutei;
use reqwest::blocking::{Client, ClientBuilder};
use serde::Serialize;
use serde_json::Serializer;
use tempfile::TempDir;

pub const DEBUG: bool = false;

fn http_client() -> &'static Client {
    static HTTP_CLIENT: OnceLock<Client> = OnceLock::new();
    HTTP_CLIENT.get_or_init(|| ClientBuilder::new().cookie_store(true).build().unwrap())
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

    /// Don't update the cards info
    #[arg(long)]
    skip_update: bool,

    /// Update the yuyu-tei.jp urls for the cards. can only be use when all cards are searched
    #[arg(long)]
    yuyutei: Option<Option<YuyuteiMode>>,

    /// [deprecated] Use holoDelta to import missing/unreleased cards data. The file that contains the card database for holoDelta
    #[arg(long)]
    holodelta_db_path: Option<PathBuf>,

    /// Use holoDelta to import missing/unreleased cards data. The folder that contains the source code for holoDelta
    #[arg(long)]
    holodelta_path: Option<PathBuf>,

    // Use the official holoLive website to import missing/unreleased cards data
    #[arg(long)]
    official_hololive: bool,

    /// Use ogbajoj's sheet to import english translations
    #[arg(long)]
    ogbajoj_sheet: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum YuyuteiMode {
    /// Use the first urls found
    Quick,
    /// Compare images to find the best match
    Images,
}

fn main() {
    dotenvy::dotenv().ok();
    let args = Args::parse();

    let mut all_cards: CardsDatabase = CardsDatabase::new();

    // create a temporary folder for the zip file content
    let temp = args.zip_images.then_some(TempDir::new().unwrap());
    let assets_path = if let Some(temp) = &temp {
        temp.path()
    } else {
        args.assets_path.as_path()
    };

    let card_mapping_file = assets_path.join("hocg_cards.json");
    let images_jp_path = assets_path.join("img");
    let images_en_path = assets_path.join("img_en");

    // load file
    if !args.clean {
        if let Ok(s) = fs::read_to_string(&card_mapping_file) {
            all_cards = serde_json::from_str(&s).unwrap();
        }
    }

    // (card number, illustration index)
    let filtered_cards: Vec<(String, usize)> = if args.skip_update {
        all_cards
            .values()
            .flat_map(|cs| cs.illustrations.iter().enumerate())
            // don't include unreleased cards
            .filter(|c| c.1.manage_id.is_some())
            .map(|c| (c.1.card_number.clone(), c.0))
            .collect()
    } else {
        // import cards info from Deck Log
        retrieve_card_info_from_decklog(
            &mut all_cards,
            &args.number_filter,
            &args.expansion,
            args.optimized_original_images,
        )
    };

    // add official images
    if args.download_images {
        download_images(
            &filtered_cards,
            &images_jp_path,
            &mut all_cards,
            args.force_download,
            args.optimized_original_images,
        );
    }

    // add proxy images
    if let Some(path) = args.proxy_path {
        prepare_en_proxy_images(&filtered_cards, &images_en_path, &mut all_cards, path);
    }

    // update yuyutei price
    if let Some(mode) = args.yuyutei {
        if args.number_filter.is_some() || args.expansion.is_some() {
            eprintln!("WARNING: SKIPPING YUYUTEI. ONLY AVAILABLE WHEN SEARCHING ALL CARDS.");
        } else {
            yuyutei(
                &mut all_cards,
                mode.unwrap_or(YuyuteiMode::Quick),
                &images_jp_path,
            );
        }
    }

    // import from holoDelta
    if let Some(holodelta_db_path) = args.holodelta_db_path {
        import_holodelta_db(&mut all_cards, &images_jp_path, &holodelta_db_path);
    } else if let Some(holodelta_path) = args.holodelta_path {
        import_holodelta(&mut all_cards, &images_jp_path, &holodelta_path);
    }

    // import from official holoLive
    if args.official_hololive {
        retrieve_card_info_from_hololive(&mut all_cards);
    }

    // import from ogbajoj
    if args.ogbajoj_sheet {
        retrieve_card_info_from_ogbajoj_sheet(&mut all_cards);
    }

    // save file
    if let Some(parent) = Path::new(&card_mapping_file).parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let mut json = vec![];
    let formatter = PrettyCompactFormatter::new();
    let mut ser = Serializer::with_formatter(&mut json, formatter);
    all_cards.serialize(&mut ser).unwrap();
    fs::write(card_mapping_file, json).unwrap();

    if args.zip_images {
        zip_images(
            &format!(
                "{}-images",
                args.expansion.as_deref().unwrap_or("hocg").to_lowercase()
            ),
            &args.assets_path,
            &images_jp_path,
        );
    }

    println!("done");
}
