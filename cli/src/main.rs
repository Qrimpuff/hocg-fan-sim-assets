use std::{
    collections::HashSet,
    fs::{self},
    path::{Path, PathBuf},
};

use clap::Parser;
use hocg_fan_sim_assets_cli::img_hash::{
    ImageGrids, can_merge, hash_image_language, is_similar, strip_grid, to_image_hash,
};
use hocg_fan_sim_assets_cli::{
    DEBUG, Language, PriceCheckMode,
    data::{
        deck_log::retrieve_card_info_from_deck_log, fill_cheer_basics,
        hololive_official::retrieve_card_info_from_hololive,
    },
    holoburi::import_holoburi,
    holodelta::{import_holodelta, import_holodelta_db},
    images::{PROXIES_FOLDER, download_images, prepare_en_proxy_images, zip_images},
    ogbajoj::{
        download_images_from_ogbajoj_sheet, retrieve_card_info_from_ogbajoj_sheet,
        retrieve_product_info_from_ogbajoj_sheet, retrieve_qna_from_ogbajoj_sheet,
    },
    price_check::{tcgplayer, yuyutei},
    products::{fix_promos_in_boosters, remove_unused_products},
    qna::generate_qna,
};
use hocg_fan_sim_assets_model::{self as hocg, CardsDatabase, ProductsDatabase, QnaDatabase};
use itertools::Itertools;
use json_pretty_compact::PrettyCompactFormatter;
use rayon::iter::{IntoParallelIterator, ParallelIterator};
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

    /// Use holoBuri to import art keys (e.g. SR_2) for cards. Downloads the art from holoburi.com
    #[arg(long)]
    holoburi: bool,

    /// Use the data from Deck Log to import cards data
    #[arg(long)]
    deck_log: bool,

    // Use the official holoLive website to import cards data
    #[arg(long)]
    hololive: bool,

    /// The language of the cards to import
    #[arg(long, default_value = "all")]
    language: Language,

    /// Use ogbajoj's sheet to import english translations (also imports the product info)
    #[arg(long)]
    ogbajoj_sheet: bool,

    /// Use ogbajoj's sheet to import the product info: sources of the promo cards (with their PR
    /// packs), english names of japanese only products. Included in --ogbajoj-sheet
    #[arg(long)]
    ogbajoj_products: bool,

    /// Remove unused assets
    #[arg(long)]
    gc: bool,

    /// Recompute stored image hashes and similarity indices from local images.
    /// This can renumber existing similarity indices.
    #[arg(long, conflicts_with_all = ["qna", "zip_images"])]
    rebuild_image_similarity: bool,

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
    let products_mapping_file = assets_path.join("hocg_products.json");
    let images_jp_path = assets_path.join("img");
    let images_en_path = assets_path.join("img_en");
    // the grids of the images, made when they are needed to compare illustrations
    let image_grids = ImageGrids::new(&images_jp_path, &images_en_path);
    let ogbajoj_sheet_cache =
        (args.ogbajoj_sheet || args.ogbajoj_products).then(|| TempDir::new().unwrap());
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
    // grids are made from the images, older hashes still have one
    for illust in all_cards
        .values_mut()
        .flat_map(|card| card.illustrations.iter_mut())
    {
        strip_grid(&mut illust.img_hash);
    }

    // the products catalog is rebuilt from the official site when missing
    let mut all_products = ProductsDatabase::new();
    if !args.clean
        && let Ok(s) = fs::read_to_string(&products_mapping_file)
    {
        match serde_json::from_str(&s) {
            Ok(products) => all_products = products,
            Err(err) => eprintln!(
                "Warning: cannot read {}, the products are retrieved again: {err}",
                products_mapping_file.display()
            ),
        }
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
                        &mut all_products,
                        &args.number_filter,
                        &args.expansion,
                        args.optimized_original_images,
                        match language {
                            Language::English => &images_en_path,
                            _ => &images_jp_path,
                        },
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

    // import promo sources (with the PR packs) and product names from ogbajoj, after the sheet
    // images are hashed
    if args.ogbajoj_sheet || args.ogbajoj_products {
        retrieve_product_info_from_ogbajoj_sheet(
            &mut all_cards,
            &mut all_products,
            &image_grids,
            ogbajoj_sheet_cache_dir,
        );
    }

    // check tags consistency
    if args.hololive || args.ogbajoj_sheet {
        check_tags_consistency(&all_cards);
    }

    if args.rebuild_image_similarity {
        rebuild_image_hashes(&mut all_cards, &image_grids)
            .unwrap_or_else(|error| panic!("cannot rebuild image similarity: {error}"));
    }

    // the basic name and text of the cheers, for the ones the official site doesn't have them for
    let filled_count = fill_cheer_basics(&mut all_cards);
    if filled_count > 0 {
        println!("Filled {filled_count} cheer names and texts");
    }

    // merge english cards
    merge_similar_cards(&mut all_cards, &image_grids);

    // promos listed in a booster by the official site of a language
    let fixed_count = fix_promos_in_boosters(&mut all_cards, &all_products);
    if fixed_count > 0 {
        println!("Fixed the products of {fixed_count} promos listed in a booster");
    }

    // identify similar looking cards by image hash
    update_similarity_index(&mut all_cards, &image_grids);

    // add proxy images
    if let Some(proxy_path) = args.proxy_path {
        prepare_en_proxy_images(&images_en_path, &mut all_cards, &proxy_path);
    }

    // update yuyutei price
    if let Some(mode) = args.yuyutei {
        if args.number_filter.is_some() || args.expansion.is_some() {
            eprintln!("WARNING: SKIPPING YUYUTEI. ONLY AVAILABLE WHEN SEARCHING ALL CARDS.");
        } else {
            yuyutei(
                &mut all_cards,
                mode.unwrap_or(PriceCheckMode::Quick),
                &image_grids,
            );
        }
    }

    // update tcgplayer product IDs
    if let Some(mode) = args.tcgplayer {
        if args.number_filter.is_some() || args.expansion.is_some() {
            eprintln!("WARNING: SKIPPING TCGPLAYER. ONLY AVAILABLE WHEN SEARCHING ALL CARDS.");
        } else {
            tcgplayer(
                &mut all_cards,
                mode.unwrap_or(PriceCheckMode::Quick),
                &image_grids,
            );
        }
    }

    // import from holoDelta
    if let Some(holodelta_db_path) = args.holodelta_db_path {
        import_holodelta_db(&mut all_cards, &holodelta_db_path, &image_grids);
    } else if let Some(holodelta_path) = args.holodelta_path {
        import_holodelta(&mut all_cards, &holodelta_path, &image_grids);
    }

    // import art keys from holoBuri
    if args.holoburi {
        import_holoburi(&mut all_cards, &image_grids);
    }

    // garbage collection
    if args.gc {
        garbage_collection(
            &mut all_cards,
            &mut all_products,
            &args.assets_path,
            &[
                &card_mapping_file,
                &qna_mapping_file,
                &products_mapping_file,
            ],
            &images_jp_path,
            &images_en_path,
        );
    }

    // previews of the images, once they are all saved
    update_placeholders(&mut all_cards, &image_grids);

    // save file
    if let Some(parent) = Path::new(&card_mapping_file).parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let mut json = vec![];
    let formatter = PrettyCompactFormatter::new();
    let mut ser = Serializer::with_formatter(&mut json, formatter);
    all_cards.serialize(&mut ser).unwrap();
    fs::write(&card_mapping_file, json).unwrap();

    // save the products catalog, sorted by expansion code
    let mut json = vec![];
    let formatter = PrettyCompactFormatter::new();
    let mut ser = Serializer::with_formatter(&mut json, formatter);
    all_products.serialize(&mut ser).unwrap();
    fs::write(&products_mapping_file, json).unwrap();

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
    all_products: &mut ProductsDatabase,
    assets_path: &Path,
    // the data files, kept
    data_files: &[&Path],
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

    // remove unknown file references
    all_cards.values_mut().for_each(|cs| {
        cs.illustrations.iter_mut().for_each(|i| {
            // japanese image
            if !images_jp_path
                .join(i.img_path.japanese.as_deref().unwrap_or_default())
                .exists()
            {
                i.img_path.japanese = None;
            }

            // english image
            if !images_en_path
                .join(i.img_path.english.as_deref().unwrap_or_default())
                .exists()
            {
                i.img_path.english = None;
            }
        });
    });

    // remove unreferenced images
    // remove products no printing refers to
    for code in remove_unused_products(all_products, all_cards) {
        println!("Removing product: {code}");
    }

    let mut required_paths: HashSet<_> = data_files.iter().map(|f| f.to_path_buf()).collect();
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

fn merge_similar_cards(all_cards: &mut CardsDatabase, grids: &ImageGrids) {
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
                } else if can_merge(&illustrations[i], &illustrations[j], grids) {
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

                        // merge products
                        if let Some(products_j) =
                            illustrations[j].products.value_mut(language).take()
                        {
                            let products = illustrations[i]
                                .products
                                .value_mut(language)
                                .get_or_insert_default();
                            for product in products_j {
                                if !products.contains(&product) {
                                    products.push(product);
                                }
                            }
                        }

                        // merge promo sources
                        let sources_j = illustrations[j].promo_sources.value_mut(language).take();
                        merge_values(&mut illustrations[i].promo_sources, language, sources_j);
                    }

                    // merge other fields
                    if illustrations[i].img_hash.is_empty() {
                        illustrations[i].img_hash = illustrations[j].img_hash.clone();
                        illustrations[i].similarity_index = illustrations[j].similarity_index;
                    }
                    if illustrations[i]
                        .illustrator
                        .as_ref()
                        .is_none_or(|i| i.is_empty())
                    {
                        illustrations[i].illustrator = illustrations[j].illustrator.clone();
                    }
                    if illustrations[i].delta_art_index.is_none() {
                        illustrations[i].delta_art_index = illustrations[j].delta_art_index;
                    }
                    if illustrations[i].holoburi_art_key.is_none() {
                        illustrations[i].holoburi_art_key =
                            illustrations[j].holoburi_art_key.clone();
                    }

                    // merge yuyutei sell paths
                    let yuyutei_paths_j = illustrations[j]
                        .yuyutei_sell_paths
                        .take()
                        .into_iter()
                        .flatten();
                    illustrations[i]
                        .yuyutei_sell_paths
                        .get_or_insert_default()
                        .extend(yuyutei_paths_j);
                    illustrations[i]
                        .yuyutei_sell_paths
                        .take_if(|v| v.is_empty());

                    // merge tcgplayer product ids
                    let tcgplayer_ids_j = illustrations[j]
                        .tcgplayer_product_ids
                        .take()
                        .into_iter()
                        .flatten();
                    illustrations[i]
                        .tcgplayer_product_ids
                        .get_or_insert_default()
                        .extend(tcgplayer_ids_j);
                    illustrations[i]
                        .tcgplayer_product_ids
                        .take_if(|v| v.is_empty());

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

/// Adds the values that are not there yet, in order.
fn merge_values(
    into: &mut hocg::Localized<Vec<String>>,
    language: hocg::Language,
    from: Option<Vec<String>>,
) {
    let Some(from) = from else {
        return;
    };
    let into = into.value_mut(language).get_or_insert_default();
    for value in from {
        if !into.contains(&value) {
            into.push(value);
        }
    }
}

/// Update the similarity index for each illustration based on other illustrations having a similar image hash.
fn update_similarity_index(all_cards: &mut CardsDatabase, grids: &ImageGrids) {
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
                if is_similar(&card.illustrations[i], &card.illustrations[j], grids) {
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

/// Recomputes the hash of every official illustration from its saved image, then resets
/// the similarity indices so that they are regrouped. Unreleased illustrations keep their
/// hash, which comes from where their image was found (e.g. a sheet thumbnail).
/// Nothing is changed if an image cannot be read. The grids of the images are kept for
/// the regrouping.
fn rebuild_image_hashes(all_cards: &mut CardsDatabase, grids: &ImageGrids) -> Result<(), String> {
    let images = all_cards
        .iter()
        .flat_map(|(number, card)| {
            card.illustrations
                .iter()
                .enumerate()
                .filter(|(_, illust)| hash_image_language(illust).is_some())
                .map(move |(index, illust)| (number, index, grids.image_of(illust).unwrap()))
        })
        .collect::<Vec<_>>();
    let updates = images
        .into_par_iter()
        .map(|(number, index, (language, path))| {
            let image = image::open(&path)
                .map_err(|error| format!("failed to open {}: {error}", path.display()))?
                .into_rgb8();
            grids.remember(&path, &image);
            Ok((number.clone(), index, to_image_hash(&image, Some(language))))
        })
        .collect::<Result<Vec<_>, String>>()?;
    println!("Rebuilt {} image hashes", updates.len());
    for (number, index, hash) in updates {
        all_cards.get_mut(&number).unwrap().illustrations[index].img_hash = hash;
    }
    for illust in all_cards
        .values_mut()
        .flat_map(|card| card.illustrations.iter_mut())
        .filter(|illust| !illust.img_hash.is_empty())
    {
        illust.similarity_index = 0;
    }
    Ok(())
}

/// Sets the placeholder of every illustration from its saved image (see
/// [`ImageGrids::image_of`]), or clears it when there is none. It is made again on every run,
/// so it follows the images whichever step changed them; images already decoded during the
/// run are not decoded again.
fn update_placeholders(all_cards: &mut CardsDatabase, grids: &ImageGrids) {
    all_cards
        .values_mut()
        .flat_map(|card| card.illustrations.iter_mut())
        .collect_vec()
        .into_par_iter()
        .for_each(|illust| illust.img_placeholder = grids.placeholder(illust).unwrap_or_default());
}

#[cfg(test)]
mod image_similarity_tests {
    use super::*;
    use hocg::{Card, CardIllustration, CardRarity, Localized};
    use hocg_fan_sim_assets_cli::img_hash::to_placeholder;

    fn card_with_images() -> CardsDatabase {
        let illustrations = [
            ("hBP09-105_U.webp", CardRarity::Uncommon),
            ("hBP09-105_P.webp", CardRarity::Promo),
        ]
        .into_iter()
        .zip(1..)
        .map(|((path, rarity), manage_id)| CardIllustration {
            card_number: "hBP09-105".to_owned(),
            rarity,
            manage_id: Localized::jp(std::collections::BTreeSet::from([manage_id])),
            img_path: Localized::jp(path.to_owned()),
            img_hash: "old hash".to_owned(),
            similarity_index: 1,
            ..Default::default()
        })
        .collect();
        [(
            "hBP09-105".to_owned(),
            Card {
                card_number: "hBP09-105".to_owned(),
                illustrations,
                ..Default::default()
            },
        )]
        .into_iter()
        .collect()
    }

    fn test_images() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/card_images")
    }

    #[test]
    fn rebuild_regroups_existing_indices() {
        let mut cards = card_with_images();
        let images = test_images();
        let grids = ImageGrids::new(&images, &images);
        rebuild_image_hashes(&mut cards, &grids).unwrap();
        update_similarity_index(&mut cards, &grids);
        let illustrations = &cards["hBP09-105"].illustrations;
        assert_ne!(
            illustrations[0].similarity_index,
            illustrations[1].similarity_index
        );
        assert!(!is_similar(&illustrations[0], &illustrations[1], &grids));
    }

    #[test]
    fn placeholders_come_from_the_saved_images() {
        let mut cards = card_with_images();
        let images = test_images();
        let grids = ImageGrids::new(&images, &images);
        cards.get_mut("hBP09-105").unwrap().illustrations[1]
            .img_path
            .japanese = Some("missing-image.webp".to_owned());
        cards.get_mut("hBP09-105").unwrap().illustrations[1].img_placeholder = "stale".to_owned();
        update_placeholders(&mut cards, &grids);
        let illustrations = &cards["hBP09-105"].illustrations;
        let image = image::open(images.join("hBP09-105_U.webp"))
            .unwrap()
            .into_rgb8();
        assert_eq!(illustrations[0].img_placeholder, to_placeholder(&image));
        // no saved image, no preview
        assert_eq!(illustrations[1].img_placeholder, "");
    }

    #[test]
    fn rebuild_does_not_change_cards_if_an_image_is_missing() {
        let mut cards = card_with_images();
        let images = test_images();
        cards.get_mut("hBP09-105").unwrap().illustrations[1]
            .img_path
            .japanese = Some("missing-image.webp".to_owned());
        let expected = cards.clone();
        let grids = ImageGrids::new(&images, &images);
        assert!(rebuild_image_hashes(&mut cards, &grids).is_err());
        assert_eq!(cards, expected);
    }
}
