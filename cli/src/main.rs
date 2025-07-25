mod data;
mod holodelta;
mod images;
mod price_check;
mod utils;

use std::{
    collections::HashSet,
    fs::{self},
    path::{Path, PathBuf},
    sync::OnceLock,
};

use clap::Parser;
use clap::ValueEnum;
use hocg_fan_sim_assets_model::CardsDatabase;
use itertools::Itertools;
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
    images::{download_images, prepare_en_proxy_images, utils::is_similar, zip_images},
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

    /// The language of the cards to import
    #[arg(long, default_value = "all")]
    language: Language,

    /// Use ogbajoj's sheet to import english translations
    #[arg(long)]
    ogbajoj_sheet: bool,

    /// Remove unused assets
    #[arg(long)]
    gc: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Language {
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
            .filter(|c| c.1.manage_id.has_value())
            .map(|c| (c.1.card_number.clone(), c.0))
            .collect()
    } else {
        // import cards info from Deck Log
        [Language::Japanese, Language::English]
            .into_iter()
            .filter(|language| args.language == Language::All || args.language == *language)
            .flat_map(|language| {
                retrieve_card_info_from_decklog(
                    &mut all_cards,
                    &args.number_filter,
                    &args.expansion,
                    args.optimized_original_images,
                    language.into(),
                )
            })
            .collect()
    };

    // add official images
    if args.download_images {
        [Language::Japanese, Language::English]
            .into_iter()
            .filter(|language| args.language == Language::All || args.language == *language)
            .for_each(|language| {
                download_images(
                    &filtered_cards,
                    &images_jp_path,
                    &images_en_path,
                    &mut all_cards,
                    args.force_download,
                    args.optimized_original_images,
                    language.into(),
                )
            });
    }

    // import from official holoLive
    if args.official_hololive {
        // official Japanese text
        if args.language == Language::All || args.language == Language::Japanese {
            retrieve_card_info_from_hololive(&mut all_cards, Language::Japanese.into());
        }
    }

    // import from ogbajoj
    if args.ogbajoj_sheet {
        retrieve_card_info_from_ogbajoj_sheet(&mut all_cards);
    }

    // import from official holoLive
    if args.official_hololive {
        // official English text, overwrite ogbajoj sheet
        if args.language == Language::All || args.language == Language::English {
            retrieve_card_info_from_hololive(&mut all_cards, Language::English.into());
        }
    }

    // check tags consistency
    if args.official_hololive || args.ogbajoj_sheet {
        check_tags_consistency(&all_cards);
    }

    // merge english cards
    merge_similar_cards(&mut all_cards);

    // add proxy images
    if let Some(proxy_path) = args.proxy_path {
        prepare_en_proxy_images(&images_en_path, &mut all_cards, &proxy_path);
    }

    // update yuyutei price
    if let Some(mode) = args.yuyutei {
        if args.number_filter.is_some() || args.expansion.is_some() {
            eprintln!("WARNING: SKIPPING YUYUTEI. ONLY AVAILABLE WHEN SEARCHING ALL CARDS.");
        } else {
            yuyutei(&mut all_cards, mode.unwrap_or(YuyuteiMode::Quick));
        }
    }

    // import from holoDelta
    if let Some(holodelta_db_path) = args.holodelta_db_path {
        import_holodelta_db(&mut all_cards, &holodelta_db_path);
    } else if let Some(holodelta_path) = args.holodelta_path {
        import_holodelta(&mut all_cards, &holodelta_path);
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

fn check_tags_consistency(all_cards: &CardsDatabase) {
    // check for tags consistency
    let tags_mapping = all_cards
        .values()
        .flat_map(|c| &c.tags)
        .filter(|t| t.japanese.is_some() && t.english.is_some())
        .map(|t| (&t.japanese, &t.english))
        .unique()
        .into_group_map_by(|t| t.0);
    for (tag, names) in tags_mapping {
        if names.len() > 1 {
            println!("Tag {tag:?} has multiple names: {names:#?}");
        }
    }
}

fn merge_similar_cards(all_cards: &mut CardsDatabase) {
    // merge similar cards by images
    let mut merged_count = 0;
    for cs in all_cards.values_mut() {
        let illustrations = &mut cs.illustrations;
        let mut i = 0;
        while i < illustrations.len() {
            let mut j = i + 1;
            while j < illustrations.len() {
                // skip if they are not the same card
                if illustrations[i].card_number != illustrations[j].card_number
                    || illustrations[i].rarity != illustrations[j].rarity
                {
                    j += 1;
                } else if is_similar(&illustrations[i], &illustrations[j]) {
                    if DEBUG {
                        println!(
                            "Merging similar images: {:?} and {:?}",
                            illustrations[i].manage_id, illustrations[j].manage_id
                        );
                    }

                    // merge the data
                    if illustrations[i].manage_id.japanese.is_none() {
                        illustrations[i].manage_id.japanese = illustrations[j].manage_id.japanese;
                    }
                    if illustrations[i].manage_id.english.is_none() {
                        illustrations[i].manage_id.english = illustrations[j].manage_id.english;
                    }
                    if illustrations[i].illustrator.is_none() {
                        illustrations[i].illustrator = illustrations[j].illustrator.clone();
                    }
                    if illustrations[i].img_path.japanese.is_none() {
                        illustrations[i].img_path.japanese =
                            illustrations[j].img_path.japanese.clone();
                    }
                    if illustrations[i].img_path.english.is_none() {
                        illustrations[i].img_path.english =
                            illustrations[j].img_path.english.clone();
                    }
                    if illustrations[i].img_last_modified.japanese.is_none() {
                        illustrations[i].img_last_modified.japanese =
                            illustrations[j].img_last_modified.japanese.clone();
                    }
                    if illustrations[i].img_last_modified.english.is_none() {
                        illustrations[i].img_last_modified.english =
                            illustrations[j].img_last_modified.english.clone();
                    }
                    if illustrations[i].img_hash.is_empty() {
                        illustrations[i].img_hash = illustrations[j].img_hash.clone();
                    }
                    if illustrations[i].yuyutei_sell_url.is_none() {
                        illustrations[i].yuyutei_sell_url =
                            illustrations[j].yuyutei_sell_url.clone();
                    }
                    if illustrations[i].delta_art_index.is_none() {
                        illustrations[i].delta_art_index = illustrations[j].delta_art_index;
                    }

                    // remove merged illustration
                    illustrations.remove(j);
                    merged_count += 1;
                } else {
                    j += 1;
                }
            }
            i += 1;
        }
    }
    println!("Merged {merged_count} similar images");
}
