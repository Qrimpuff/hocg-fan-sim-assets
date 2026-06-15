use std::{
    collections::HashSet,
    fs::{self},
    path::{Path, PathBuf},
};

use clap::Parser;
use hocg_fan_sim_assets_cli::{
    DEBUG, Language, PriceCheckMode,
    data::{
        deck_log::retrieve_card_info_from_deck_log,
        hololive_official::retrieve_card_info_from_hololive,
    },
    holodelta::{import_holodelta, import_holodelta_db},
    images::{PROXIES_FOLDER, download_images, prepare_en_proxy_images, zip_images},
    ogbajoj::{
        download_images_from_ogbajoj_sheet, retrieve_card_info_from_ogbajoj_sheet,
        retrieve_qna_from_ogbajoj_sheet,
    },
    price_check::{tcgplayer, yuyutei},
    qna::generate_qna,
};
use hocg_fan_sim_assets_model::{
    self as hocg, CardsDatabase, QnaDatabase,
    img_hash::{can_merge, is_similar},
};
use itertools::Itertools;
use json_pretty_compact::PrettyCompactFormatter;
use serde::Serialize;
use serde_json::Serializer;
use tempfile::TempDir;
use walkdir::WalkDir;

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

    /// Update the yuyu-tei.jp urls for the cards. can only be use when all cards are searched
    #[arg(long)]
    yuyutei: Option<Option<PriceCheckMode>>,

    /// Update the TCGplayer product IDs for the cards. can only be use when all cards are searched
    #[arg(long)]
    tcgplayer: Option<Option<PriceCheckMode>>,

    /// [deprecated] Use holoDelta to import missing/unreleased cards data. The file that contains the card database for holoDelta
    #[arg(long)]
    holodelta_db_path: Option<PathBuf>,

    /// Use holoDelta to import missing/unreleased cards data. The folder that contains the source code for holoDelta
    #[arg(long)]
    holodelta_path: Option<PathBuf>,

    /// Use the data from Deck Log to import cards data
    #[arg(long)]
    deck_log: bool,

    // Use the official holoLive website to import cards data
    #[arg(long)]
    hololive: bool,

    /// The language of the cards to import
    #[arg(long, default_value = "all")]
    language: Language,

    /// Use ogbajoj's sheet to import english translations
    #[arg(long)]
    ogbajoj_sheet: bool,

    /// Remove unused assets
    #[arg(long)]
    gc: bool,

    /// Generate Q&A file from official sources. Does not rely on asset files.
    /// Compatible with --clean and --ogbajoj-sheet.
    /// Doesn't update assets.
    #[arg(long)]
    qna: bool,
}

fn main() {
    dotenvy::dotenv().ok();
    let args = Args::parse();

    // create a temporary folder for the zip file content
    let temp = args.zip_images.then_some(TempDir::new().unwrap());
    let assets_path = if let Some(temp) = &temp {
        temp.path()
    } else {
        args.assets_path.as_path()
    };

    let card_mapping_file = assets_path.join("hocg_cards.json");
    let qna_mapping_file = assets_path.join("hocg_qnas.json");
    let images_jp_path = assets_path.join("img");
    let images_en_path = assets_path.join("img_en");
    let ogbajoj_sheet_cache = args.ogbajoj_sheet.then(|| TempDir::new().unwrap());
    let ogbajoj_sheet_cache_dir = ogbajoj_sheet_cache.as_ref().map(|cache| cache.path());

    // Q&As
    if args.qna {
        let mut all_qnas: QnaDatabase = QnaDatabase::new();

        // load file
        if !args.clean
            && let Ok(s) = fs::read_to_string(&qna_mapping_file)
        {
            all_qnas = serde_json::from_str(&s).unwrap();
        }

        // import Q&As
        // the english Q&As on official site are of lower quality, so only import japanese Q&As by default
        [Language::Japanese]
            .into_iter()
            .filter(|language| args.language == Language::All || args.language == *language)
            .for_each(|language| {
                generate_qna(&mut all_qnas, language.into());
            });

        // import from ogbajoj
        if args.ogbajoj_sheet {
            retrieve_qna_from_ogbajoj_sheet(&mut all_qnas, ogbajoj_sheet_cache_dir);
        }

        // save file
        if let Some(parent) = Path::new(&qna_mapping_file).parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut json = vec![];
        let formatter = PrettyCompactFormatter::new();
        let mut ser = Serializer::with_formatter(&mut json, formatter);
        all_qnas.serialize(&mut ser).unwrap();
        fs::write(&qna_mapping_file, json).unwrap();

        println!("done");
        return;
    }

    let mut all_cards: CardsDatabase = CardsDatabase::new();

    // load file
    if !args.clean
        && let Ok(s) = fs::read_to_string(&card_mapping_file)
    {
        all_cards = serde_json::from_str(&s).unwrap();
    }

    // (card number, illustration index)
    let mut filtered_cards: HashSet<(String, usize)> = HashSet::new();

    // import cards info from Deck Log
    if args.deck_log {
        filtered_cards.extend(
            [Language::Japanese, Language::English]
                .into_iter()
                .filter(|language| args.language == Language::All || args.language == *language)
                .flat_map(|language| {
                    retrieve_card_info_from_deck_log(
                        &mut all_cards,
                        &args.number_filter,
                        &args.expansion,
                        args.optimized_original_images,
                        &images_jp_path,
                        &images_en_path,
                        language.into(),
                    )
                }),
        );
    }

    // import from official holoLive
    if args.hololive {
        filtered_cards.extend(
            [Language::Japanese, Language::English]
                .into_iter()
                .filter(|language| args.language == Language::All || args.language == *language)
                .flat_map(|language| {
                    retrieve_card_info_from_hololive(
                        &mut all_cards,
                        &args.number_filter,
                        &args.expansion,
                        args.optimized_original_images,
                        &images_jp_path,
                        &images_en_path,
                        language.into(),
                    )
                }),
        )
    }

    // if no filter is specified, add all cards to the filtered list
    if filtered_cards.is_empty() && args.number_filter.is_none() && args.expansion.is_none() {
        filtered_cards.extend(
            all_cards
                .values()
                .flat_map(|cs| cs.illustrations.iter().enumerate())
                // don't include unreleased cards
                .filter(|c| c.1.manage_id.has_value())
                .map(|c| (c.1.card_number.clone(), c.0)),
        );
    }

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

    // download images from ogbajoj sheet
    if args.ogbajoj_sheet {
        download_images_from_ogbajoj_sheet(
            &images_jp_path,
            &images_en_path,
            &mut all_cards,
            ogbajoj_sheet_cache_dir,
        );
    }

    // import from ogbajoj
    if args.ogbajoj_sheet {
        retrieve_card_info_from_ogbajoj_sheet(&mut all_cards, ogbajoj_sheet_cache_dir);
    }

    // check tags consistency
    if args.hololive || args.ogbajoj_sheet {
        check_tags_consistency(&all_cards);
    }

    // merge english cards
    merge_similar_cards(&mut all_cards);

    // identify similar looking cards by image hash
    update_similarity_index(&mut all_cards);

    // add proxy images
    if let Some(proxy_path) = args.proxy_path {
        prepare_en_proxy_images(&images_en_path, &mut all_cards, &proxy_path);
    }

    // update yuyutei price
    if let Some(mode) = args.yuyutei {
        if args.number_filter.is_some() || args.expansion.is_some() {
            eprintln!("WARNING: SKIPPING YUYUTEI. ONLY AVAILABLE WHEN SEARCHING ALL CARDS.");
        } else {
            yuyutei(&mut all_cards, mode.unwrap_or(PriceCheckMode::Quick));
        }
    }

    // update tcgplayer product IDs
    if let Some(mode) = args.tcgplayer {
        if args.number_filter.is_some() || args.expansion.is_some() {
            eprintln!("WARNING: SKIPPING TCGPLAYER. ONLY AVAILABLE WHEN SEARCHING ALL CARDS.");
        } else {
            tcgplayer(&mut all_cards, mode.unwrap_or(PriceCheckMode::Quick));
        }
    }

    // import from holoDelta
    if let Some(holodelta_db_path) = args.holodelta_db_path {
        import_holodelta_db(&mut all_cards, &holodelta_db_path);
    } else if let Some(holodelta_path) = args.holodelta_path {
        import_holodelta(&mut all_cards, &holodelta_path);
    }

    // garbage collection
    if args.gc {
        garbage_collection(
            &mut all_cards,
            &args.assets_path,
            &card_mapping_file,
            &qna_mapping_file,
            &images_jp_path,
            &images_en_path,
        );
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
    all_cards: &mut CardsDatabase,
    assets_path: &Path,
    card_mapping_file: &Path,
    qna_mapping_file: &Path,
    images_jp_path: &Path,
    images_en_path: &Path,
) {
    println!("Running garbage collection...");

    // remove unreferenced cards
    all_cards.values_mut().for_each(|cs| {
        cs.illustrations.retain(|i| {
            let keep = i.manage_id.has_value() || i.ogbajoj_sheet_cells.is_some();
            if !keep {
                println!("Removing illustration: {} - {}", cs.card_number, i.rarity);
            }
            keep
        });
    });
    all_cards.retain(|_, cs| {
        let keep = !cs.illustrations.is_empty();
        if !keep {
            println!("Removing card: {}", cs.card_number);
        }
        keep
    });

    // remove unreferenced images
    let mut required_paths =
        HashSet::from([card_mapping_file.to_owned(), qna_mapping_file.to_owned()]);
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
    // sort cards by rarity, then by semi chronological manage id (JP/EN)
    for cs in all_cards.values_mut() {
        cs.illustrations.sort_by_cached_key(|a| {
            (
                a.rarity.clone(),
                a.manage_id
                    .japanese
                    .as_ref()
                    .and_then(|ids| ids.first().copied())
                    .unwrap_or(u32::MAX)
                    .min(
                        a.manage_id
                            .english
                            .as_ref()
                            .and_then(|ids| ids.first().copied())
                            .unwrap_or(u32::MAX),
                    ),
            )
        });
    }

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
                } else if can_merge(&illustrations[i], &illustrations[j]) {
                    if DEBUG {
                        println!(
                            "Merging similar images: {:?} and {:?}",
                            illustrations[i].manage_id, illustrations[j].manage_id
                        );
                    }

                    // merge the data
                    for language in [hocg::Language::Japanese, hocg::Language::English] {
                        // merge manage id
                        if let Some(manage_id) =
                            illustrations[j].manage_id.value_mut(language).take()
                        {
                            illustrations[i]
                                .manage_id
                                .value_mut(language)
                                .get_or_insert_default()
                                .extend(manage_id);
                        }

                        // merge image path and last modified
                        // overwrite if the current one is a proxy image
                        if illustrations[i].img_path.value(language).is_none()
                            || (illustrations[i]
                                .img_path
                                .value(language)
                                .as_ref()
                                .is_some_and(|p| p.starts_with(PROXIES_FOLDER))
                                && illustrations[j]
                                    .img_path
                                    .value(language)
                                    .as_ref()
                                    .is_some_and(|p| !p.starts_with(PROXIES_FOLDER)))
                        {
                            *illustrations[i].img_path.value_mut(language) =
                                illustrations[j].img_path.value(language).clone();
                            *illustrations[i].img_last_modified.value_mut(language) =
                                illustrations[j].img_last_modified.value(language).clone();
                        }
                    }

                    // merge other fields
                    if illustrations[i].img_hash.is_empty() {
                        illustrations[i].img_hash = illustrations[j].img_hash.clone();
                        illustrations[i].similarity_index = illustrations[j].similarity_index;
                    }
                    if illustrations[i].illustrator.is_none() {
                        illustrations[i].illustrator = illustrations[j].illustrator.clone();
                    }
                    if illustrations[i].yuyutei_sell_url.is_none() {
                        illustrations[i].yuyutei_sell_url =
                            illustrations[j].yuyutei_sell_url.clone();
                    }
                    if illustrations[i].tcgplayer_product_id.is_none() {
                        illustrations[i].tcgplayer_product_id =
                            illustrations[j].tcgplayer_product_id;
                    }
                    if illustrations[i].delta_art_index.is_none() {
                        illustrations[i].delta_art_index = illustrations[j].delta_art_index;
                    }

                    // merge ogbajoj sheet cells
                    let ogbajoj_sheet_cells_j = illustrations[j]
                        .ogbajoj_sheet_cells
                        .take()
                        .into_iter()
                        .flatten();
                    illustrations[i]
                        .ogbajoj_sheet_cells
                        .get_or_insert_default()
                        .extend(ogbajoj_sheet_cells_j);
                    illustrations[i]
                        .ogbajoj_sheet_cells
                        .take_if(|v| v.is_empty());

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

/// Update the similarity index for each illustration based on other illustrations having a similar image hash.
fn update_similarity_index(all_cards: &mut CardsDatabase) {
    for card in all_cards.values_mut() {
        // similarity index starts from 1, 0 means not set. the same similarity index means similar images
        let mut next_similarity_index = 1;
        let mut assigned_indices: HashSet<u32> = card
            .illustrations
            .iter()
            .map(|illust| illust.similarity_index)
            .collect();

        for i in 0..card.illustrations.len() {
            // skip if already set or doesn't have an image hash
            if card.illustrations[i].similarity_index != 0
                || card.illustrations[i].img_hash.is_empty()
            {
                continue;
            }

            for j in 0..card.illustrations.len() {
                // skip if it's the same illustration or doesn't have a similarity index
                if i == j || card.illustrations[j].similarity_index == 0 {
                    continue;
                }

                // if they are similar, assign the same similarity index
                if is_similar(&card.illustrations[i], &card.illustrations[j]) {
                    card.illustrations[i].similarity_index = card.illustrations[j].similarity_index;
                    break;
                }
            }

            // couldn't find any similar illustration, assign the first available similarity index
            if card.illustrations[i].similarity_index == 0 {
                while assigned_indices.contains(&next_similarity_index) {
                    next_similarity_index += 1;
                }
                card.illustrations[i].similarity_index = next_similarity_index;
                assigned_indices.insert(next_similarity_index);
            }
        }
    }
}
