mod data;
mod holodelta;
mod images;
mod price_check;

use std::{
    collections::HashSet,
    fs::{self},
    path::{Path, PathBuf},
    sync::OnceLock,
};

use clap::Parser;
use clap::ValueEnum;
use hocg_fan_sim_assets_model::CardsDatabase;
use json_pretty_compact::PrettyCompactFormatter;
use reqwest::blocking::{Client, ClientBuilder};
use serde::Serialize;
use serde_json::Serializer;
use tempfile::TempDir;
use walkdir::WalkDir;

use crate::{
    data::{
        decklog::retrieve_card_info_from_decklog,
        hololive_official::retrieve_card_info_from_hololive,
        ogbajoj::retrieve_card_info_from_ogbajoj_sheet,
    },
    holodelta::{import_holodelta, import_holodelta_db},
    images::{download_images, prepare_en_proxy_images, zip_images},
    price_check::yuyutei,
};

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

    /// Remove unused assets
    #[arg(long)]
    gc: bool,
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
    if let Some(proxy_path) = args.proxy_path {
        prepare_en_proxy_images(
            &filtered_cards,
            &images_en_path,
            &mut all_cards,
            &proxy_path,
            &images_jp_path,
        );
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

    // import from official holoLive
    if args.official_hololive {
        retrieve_card_info_from_hololive(&mut all_cards);
    }

    // import from ogbajoj
    if args.ogbajoj_sheet {
        retrieve_card_info_from_ogbajoj_sheet(&mut all_cards);
    }

    // import from holoDelta
    if let Some(holodelta_db_path) = args.holodelta_db_path {
        import_holodelta_db(&mut all_cards, &images_jp_path, &holodelta_db_path);
    } else if let Some(holodelta_path) = args.holodelta_path {
        import_holodelta(&mut all_cards, &images_jp_path, &holodelta_path);
    }

    // save file
    if let Some(parent) = Path::new(&card_mapping_file).parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let mut json = vec![];
    let formatter = PrettyCompactFormatter::new();
    let mut ser = Serializer::with_formatter(&mut json, formatter);
    all_cards.serialize(&mut ser).unwrap();
    fs::write(&card_mapping_file, json).unwrap();

    // garbage collection
    if args.gc {
        garbage_collection(
            &all_cards,
            &args.assets_path,
            &card_mapping_file,
            &images_jp_path,
            &images_en_path,
        );
    }

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

fn garbage_collection(
    all_cards: &CardsDatabase,
    assets_path: &Path,
    card_mapping_file: &Path,
    images_jp_path: &Path,
    images_en_path: &Path,
) {
    println!("Running garbage collection...");

    let mut required_paths = HashSet::from([card_mapping_file.to_owned()]);
    required_paths.extend(
        all_cards
            .values()
            .flat_map(|cs| cs.illustrations.iter())
            .filter_map(|c| c.img_path.japanese.as_deref())
            .map(|p| images_jp_path.join(p)),
    );
    required_paths.extend(
        all_cards
            .values()
            .flat_map(|cs| cs.illustrations.iter())
            .filter_map(|c| c.img_path.english.as_deref())
            .map(|p| images_en_path.join(p)),
    );

    // don't delete the assets folder
    for entry in WalkDir::new(images_jp_path)
        .contents_first(true)
        .into_iter()
        .flatten()
        .chain(
            WalkDir::new(images_en_path)
                .contents_first(true)
                .into_iter()
                .flatten(),
        )
    {
        // skip git directories
        if entry.path().components().any(|c| c.as_os_str() == ".git") {
            continue;
        }

        // keep referenced assets
        if required_paths.contains(entry.path()) {
            continue;
        }

        if entry.file_type().is_file() {
            // remove file
            println!(
                "Removing file: {}",
                entry.path().strip_prefix(assets_path).unwrap().display()
            );
            fs::remove_file(entry.path()).unwrap();
        } else if entry.file_type().is_dir() {
            // remove folder, if it's empty
            if entry.path().read_dir().unwrap().next().is_none() {
                println!(
                    "Removing empty folder: {}",
                    entry.path().strip_prefix(assets_path).unwrap().display()
                );
                fs::remove_dir(entry.path()).unwrap();
            }
        }
    }
}
