//! The catalog of the products cards are released in (`hocg_products.json`), from the product
//! entries of the official card pages (JP "収録商品", EN "Card Set" / "Featured Products").
//! Printings only store the expansion codes of their products.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::LazyLock;

use hocg_fan_sim_assets_model::{CardsDatabase, Language, Localized, ProductsDatabase};
use itertools::Itertools;
use regex::Regex;

use crate::DEBUG;

/// A product entry of a card detail page.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProductEntry {
    pub name: String,
    /// Expansion code, from the "CARD LIST" link of the entry.
    pub expansion: Option<String>,
    pub release_date: Option<String>,
    pub category: Option<String>,
    /// The "MORE" link of the entry.
    pub url: Option<String>,
}

/// Expansion codes of products: "h", the kind of product in 2 capital letters, then a 2 digit
/// number, a year with a name for event sets, or nothing. All the codes of the official sites
/// (2026-11) follow it: booster packs hBP01..hBP09, extra booster hEB01, start decks
/// hSD01..hSD21, event start deck set hSD2025summer, start cheer set hYS01, celebration sets
/// hCS01-hCS02, collections hCO01-hCO03 and hPC01, wafers hWF01, PR cards hPR.
static PRODUCT_CODE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^h[A-Z]{2}(?:\d{2}|\d{4}[a-z]+)?$").unwrap());

/// Expansion codes of tournament card pools, listed with the products of the cards but not
/// products: e.g. EN sele07 "Selection Cup - Diva Fever", a limited format tournament using the
/// cards of some products.
static CARD_POOL_CODE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^sele\d+$").unwrap());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpansionKind {
    Product,
    CardPool,
    /// Matches neither pattern, a new kind of code: kept as a product, with a warning.
    Unknown,
}

pub fn expansion_kind(code: &str) -> ExpansionKind {
    if PRODUCT_CODE.is_match(code) {
        ExpansionKind::Product
    } else if CARD_POOL_CODE.is_match(code) {
        ExpansionKind::CardPool
    } else {
        ExpansionKind::Unknown
    }
}

/// What a product name listed on a card is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListedProduct {
    /// A product of the catalog, by expansion code.
    Product(String),
    /// A tournament card pool, not a product.
    CardPool,
    /// Unknown: the detail page couldn't be read, or has no expansion code.
    Unknown,
}

/// The product names of the catalog in a language, to their expansion code.
pub fn catalog_names(products: &ProductsDatabase, language: Language) -> HashMap<String, String> {
    let mut names = HashMap::new();
    for (code, product) in products {
        if let Some(name) = product.name.value(language)
            && let Some(other) = names.insert(name.clone(), code.clone())
        {
            eprintln!("Warning: products {other} and {code} have the same name {name:?}");
        }
    }
    names
}

/// Whether the detail page of a listed product name has to be read: the name is not in the
/// catalog, or the product has no release date yet (e.g. PR cards, or a date not announced).
pub fn needs_details(
    products: &ProductsDatabase,
    names: &HashMap<String, String>,
    name: &str,
    language: Language,
) -> bool {
    names
        .get(name)
        .and_then(|code| products.get(code))
        .is_none_or(|product| product.release_date.value(language).is_none())
}

/// Adds the product entries of the detail pages to the catalog, and returns what each name is.
/// Values missing from a page are kept.
pub fn update_catalog(
    products: &mut ProductsDatabase,
    entries: &[ProductEntry],
    language: Language,
) -> HashMap<String, ListedProduct> {
    let mut listed = HashMap::new();
    for entry in entries {
        let Some(code) = &entry.expansion else {
            eprintln!(
                "Warning: product {:?} has no expansion code, it can't be stored",
                entry.name
            );
            listed
                .entry(entry.name.clone())
                .or_insert(ListedProduct::Unknown);
            continue;
        };
        match expansion_kind(code) {
            ExpansionKind::CardPool => {
                if DEBUG {
                    println!("Card pool, not a product: {} ({code})", entry.name);
                }
                listed.insert(entry.name.clone(), ListedProduct::CardPool);
                continue;
            }
            ExpansionKind::Unknown => eprintln!(
                "Warning: unexpected expansion code {code:?} for {:?}, kept as a product",
                entry.name
            ),
            ExpansionKind::Product => {}
        }

        let product = products.entry(code.clone()).or_default();
        *product.name.value_mut(language) = Some(entry.name.clone());
        if let Some(date) = &entry.release_date {
            *product.release_date.value_mut(language) = Some(date.clone());
        }
        if let Some(url) = &entry.url {
            *product.url.value_mut(language) = Some(url.clone());
        }
        match (&product.category, &entry.category) {
            (None, Some(category)) => product.category = Some(category.clone()),
            (Some(stored), Some(category)) if stored != category => eprintln!(
                "Warning: product {code} is {category:?} on the {language:?} site, keeping {stored:?}"
            ),
            _ => {}
        }
        listed.insert(entry.name.clone(), ListedProduct::Product(code.clone()));
    }
    listed
}

/// Updates the products of the printings from the names listed on the card list (`listed`, by
/// manage id, in page order). The stored codes are replaced only when all the printings of an
/// illustration were listed and all the names are known; otherwise the new codes are added to
/// them. Returns the number of updated illustrations and the unknown names.
pub fn update_printings(
    all_cards: &mut CardsDatabase,
    listed: &BTreeMap<u32, Vec<String>>,
    resolved: &HashMap<String, ListedProduct>,
    language: Language,
) -> (usize, BTreeSet<String>) {
    let mut updated_count = 0;
    let mut unknown_names = BTreeSet::new();
    for illust in all_cards
        .values_mut()
        .flat_map(|cs| cs.illustrations.iter_mut())
    {
        let Some(manage_ids) = illust.manage_id.value(language).clone() else {
            continue;
        };
        let names = manage_ids
            .iter()
            .filter_map(|id| listed.get(id))
            .flatten()
            .unique()
            .collect_vec();
        if names.is_empty() {
            // not in this search, or the page doesn't list products: keep what we have
            continue;
        }
        let seen_all = manage_ids.iter().all(|id| listed.contains_key(id));

        let mut codes = vec![];
        let mut complete = true;
        for name in names {
            match resolved.get(name) {
                Some(ListedProduct::Product(code)) => codes.push(code.clone()),
                Some(ListedProduct::CardPool) => {}
                Some(ListedProduct::Unknown) | None => {
                    complete = false;
                    unknown_names.insert(name.clone());
                }
            }
        }
        let codes = codes.into_iter().unique().collect_vec();

        let old = illust.products.value(language).clone().unwrap_or_default();
        let new = if seen_all && complete {
            codes
        } else {
            old.iter()
                .filter(|code| expansion_kind(code) != ExpansionKind::CardPool)
                .chain(codes.iter().filter(|code| !old.contains(code)))
                .cloned()
                .collect_vec()
        };
        if new != old {
            if DEBUG {
                println!(
                    "Products {} {}: {old:?} -> {new:?}",
                    illust.card_number, illust.rarity
                );
            }
            *illust.products.value_mut(language) = (!new.is_empty()).then_some(new);
            updated_count += 1;
        }
    }
    (updated_count, unknown_names)
}

/// A product name, for comparing names written differently (the card pages and the products
/// pages, the sources and the PR packs of the sheet): without the 【...】 notes (e.g. 【イベント物販限定商品】), punctuation, spaces and case.
pub fn comparable_name(name: &str) -> String {
    static NOTE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"【[^】]*】").unwrap());
    NOTE.replace_all(name, "")
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// The page of the products pages (`listing`: name, url) of a product name. The names don't
/// always match exactly (the products pages may add the name of the game, or drop a note), so
/// the closest of the names containing the other is taken; `None` if there is none, or several
/// as close.
pub fn listing_page<'a>(name: &str, listing: &'a [(String, String)]) -> Option<&'a str> {
    closest_listing_page(name, listing).map(|(_, url)| url)
}

/// [`listing_page`], with how far the names are (in bytes).
fn closest_listing_page<'a>(
    name: &str,
    listing: impl IntoIterator<Item = &'a (String, String)>,
) -> Option<(usize, &'a str)> {
    let name = comparable_name(name);
    if name.is_empty() {
        return None;
    }
    let candidates = listing
        .into_iter()
        .filter_map(|(listed_name, url)| {
            let listed_name = comparable_name(listed_name);
            (!listed_name.is_empty()
                && (listed_name.contains(&name) || name.contains(&listed_name)))
            .then(|| (listed_name.len().abs_diff(name.len()), url.as_str()))
        })
        .unique_by(|(_, url)| *url)
        .sorted()
        .collect_vec();
    match candidates.as_slice() {
        [candidate] => Some(*candidate),
        [best, next, ..] if best.0 < next.0 => Some(*best),
        _ => None,
    }
}

/// Fixes the product page urls of a language: the "MORE" links of the card pages are sometimes
/// dead (an old page address). A url that is not in the products pages of the site (`listing`)
/// and that `is_dead` (`None` if it can't be checked: kept) is replaced by the page of the
/// product in the listing, by name, or removed if there is none. A page is given to one product
/// only: the closest names first, and not the page of another product. Returns the number of
/// fixed products.
pub fn fix_product_urls(
    products: &mut ProductsDatabase,
    listing: &[(String, String)],
    language: Language,
    is_dead: impl Fn(&str) -> Option<bool>,
) -> usize {
    let mut taken: BTreeSet<String> = products
        .values()
        .filter_map(|product| product.url.value(language).clone())
        .collect();
    let mut dead = products
        .iter()
        .filter_map(|(code, product)| {
            let url = product.url.value(language).as_ref()?;
            (!listing.iter().any(|(_, listed)| listed == url) && is_dead(url) == Some(true))
                .then(|| code.clone())
        })
        .collect_vec();
    let fixed_count = dead.len();
    loop {
        let best = dead
            .iter()
            .enumerate()
            .filter_map(|(index, code)| {
                let name = products[code].name.value(language).as_deref()?;
                let available = listing.iter().filter(|(_, url)| !taken.contains(url));
                closest_listing_page(name, available).map(|(distance, url)| (distance, index, url))
            })
            .min();
        let Some((_, index, page)) = best else {
            break;
        };
        let code = dead.remove(index);
        let product = products.get_mut(&code).unwrap();
        let url = product.url.value(language).clone().unwrap_or_default();
        println!("Product {code}: page {url} not found, using {page}");
        *product.url.value_mut(language) = Some(page.to_owned());
        taken.insert(page.to_owned());
    }
    for code in dead {
        let url = products
            .get_mut(&code)
            .unwrap()
            .url
            .value_mut(language)
            .take();
        eprintln!(
            "Warning: product {code}: page {} not found, and not in the products pages, removed",
            url.unwrap_or_default()
        );
    }
    fixed_count
}

/// Removes the products no printing refers to. Returns their codes.
pub fn remove_unused_products(
    products: &mut ProductsDatabase,
    all_cards: &CardsDatabase,
) -> Vec<String> {
    let used: BTreeSet<&String> = all_cards
        .values()
        .flat_map(|cs| cs.illustrations.iter())
        .flat_map(|i| [&i.products.japanese, &i.products.english])
        .flatten()
        .flatten()
        .collect();
    let unused = products
        .keys()
        .filter(|code| !used.contains(code))
        .cloned()
        .collect_vec();
    for code in &unused {
        products.remove(code);
    }
    unused
}

// --- Promos in boosters ---

/// Promos are not in booster packs: a promo printing whose official page of a language lists a
/// booster (category "boosters", see [`BOOSTERS_CATEGORY`]) takes the products of the other
/// language instead, when those are not boosters (e.g. hBP07-100 P, "Booster Pack - Diva
/// Fever" on the english site, PR cards on the japanese one), with a note in the log; kept, with
/// a warning, when the other language has no products or has boosters too. Returns the number of
/// fixed printings.
pub fn fix_promos_in_boosters(all_cards: &mut CardsDatabase, products: &ProductsDatabase) -> usize {
    let is_booster = |code: &String| {
        products
            .get(code)
            .is_some_and(|p| p.category.as_deref() == Some(BOOSTERS_CATEGORY))
    };
    let mut fixed_count = 0;
    for illust in all_cards
        .values_mut()
        .flat_map(|cs| cs.illustrations.iter_mut())
        .filter(|i| i.rarity == hocg_fan_sim_assets_model::CardRarity::Promo)
    {
        for (language, other) in [
            (Language::Japanese, Language::English),
            (Language::English, Language::Japanese),
        ] {
            let Some(codes) = illust.products.value(language) else {
                continue;
            };
            if !codes.iter().any(is_booster) {
                continue;
            }
            let replacement = illust.products.value(other).clone().filter(|other_codes| {
                !other_codes.is_empty() && !other_codes.iter().any(is_booster)
            });
            match replacement {
                Some(other_codes) => {
                    // the site keeps listing it: a note, not a warning on every run
                    println!(
                        "{} {:?}: the {language:?} official page lists the promo in {codes:?}, a booster; using the {other:?} products {other_codes:?}",
                        illust.card_number, illust.manage_id
                    );
                    *illust.products.value_mut(language) = Some(other_codes);
                    fixed_count += 1;
                }
                None => crate::warn_skipped(&format!(
                    "{} {:?}: the {language:?} official page lists the promo in {codes:?}, a booster; kept, no other products for it",
                    illust.card_number, illust.manage_id
                )),
            }
        }
    }
    fixed_count
}

// --- Short names ---

/// The official category of booster packs: their short name is the set name alone, the app shows
/// it with the expansion code ("hBP01 Blooming Radiance").
const BOOSTERS_CATEGORY: &str = "boosters";

/// The official name without its wrapping punctuation. A leading sales channel note in 【】 is
/// always dropped; the name of the game is kept ("hololive OFFICIAL CARD GAME ツインウエハース"
/// stays), unless the sheet drops it, see [`update_short_names`].
/// - booster packs (category "boosters") keep the set name alone: the title in 「」
///   ("ブースターパック「ボリュームヴォルテックス」"), what follows a spaced dash ("Booster Pack –
///   Blooming Radiance"), otherwise the words after the first one ("ブースターパック
///   バウンサーバウンド", "エクストラブースター サマー・ホログラム");
/// - other products keep their kind, without the 「」 and the spaced dash: "スタートデッキ
///   ときのそら＆AZKi", "Start Deck Tokino Sora & AZKi".
///
/// `None` when the name stays the same.
pub fn short_name(name: &str, category: Option<&str>) -> Option<String> {
    let base = strip_leading_notes(name);
    let short = if category == Some(BOOSTERS_CATEGORY) {
        if let Some(title) = bracketed_title(base) {
            title.to_string()
        } else if let Some(title) = after_spaced_dash(base) {
            title.to_string()
        } else {
            let words = base.split_whitespace().collect_vec();
            if words.len() > 1 {
                words[1..].join(" ")
            } else {
                base.to_string()
            }
        }
    } else {
        static DASH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s[-–—]\s").unwrap());
        DASH.replace_all(base, " ")
            .replace(['「', '」'], " ")
            .split_whitespace()
            .join(" ")
    };
    let short = short.trim();
    (!short.is_empty() && short != name).then(|| short.to_string())
}

/// The end of `name` that is `sheet_name` (ignoring the case), after a space, if `name` has more
/// in front.
fn sheet_suffix<'a>(name: &'a str, sheet_name: &str) -> Option<&'a str> {
    let sheet_name = sheet_name.trim();
    let start = name.len().checked_sub(sheet_name.len())?;
    let tail = name.get(start..)?;
    let front = &name[..start];
    (!sheet_name.is_empty()
        && tail.eq_ignore_ascii_case(sheet_name)
        && front.ends_with(char::is_whitespace)
        && !front.trim().is_empty())
    .then_some(tail)
}

/// Without the leading notes in 【】.
fn strip_leading_notes(name: &str) -> &str {
    let mut name = name.trim();
    while let Some(rest) = name.strip_prefix('【')
        && let Some((_, rest)) = rest.split_once('】')
    {
        name = rest.trim();
    }
    name
}

/// The text in 「」.
fn bracketed_title(name: &str) -> Option<&str> {
    let (_, rest) = name.split_once('「')?;
    let (title, _) = rest.split_once('」')?;
    Some(title.trim()).filter(|t| !t.is_empty())
}

/// What follows the first dash surrounded by spaces: " – ", " - " or " — ".
fn after_spaced_dash(name: &str) -> Option<&str> {
    static DASH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s[-–—]\s").unwrap());
    let m = DASH.find(name)?;
    Some(name[m.end()..].trim()).filter(|t| !t.is_empty())
}

/// Sets the short names of the catalog from its official names (see [`short_name`]), in line with
/// the sheet: an official name that ends with the sheet's name of the product (its english
/// `unofficial_name`) is shortened to it, "【hololive production OFFICIAL SHOP限定】hololive
/// OFFICIAL CARD GAME 1st Anniversary Celebration Set" to "1st Anniversary Celebration Set" (the
/// sheet keeps the name of the game for "hololive Official Card Game Twin Wafers").
pub fn update_short_names(products: &mut ProductsDatabase) {
    for product in products.values_mut() {
        let sheet_name = product.unofficial_name.english.as_deref();
        for language in [Language::Japanese, Language::English] {
            let short = product.name.value(language).as_ref().and_then(|name| {
                let category = product.category.as_deref();
                match sheet_name.and_then(|sheet| sheet_suffix(strip_leading_notes(name), sheet)) {
                    Some(tail) => short_name(tail, category).or_else(|| Some(tail.to_string())),
                    None => short_name(name, category),
                }
            });
            *product.short_name.value_mut(language) = short;
        }
    }
}

// --- Language tags of @ogbajoj's sheet ---

/// Splits a trailing language tag from a line of the sheet: "Basic PR Pack vol 1 (JP)",
/// "Bloom Cup (EN only)", "Trio Challenge (JP and EN)". A tag is a parenthesis with only the
/// words JP, EN, "only" and "and"; other parentheses ("(JP format)") are part of the text.
/// Returns the text and the languages of the tag, if any.
pub fn split_language_tag(line: &str) -> (&str, Option<Vec<Language>>) {
    static TAG: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^(.*?)\s*\(([^()]*)\)\s*$").unwrap());
    let line = line.trim();
    let Some(captures) = TAG.captures(line) else {
        return (line, None);
    };
    let words = captures[2]
        .split(|c: char| c.is_whitespace() || c == ',' || c == '&' || c == '/')
        .filter(|w| !w.is_empty())
        .collect_vec();
    let mut languages = vec![];
    for word in &words {
        match word.to_ascii_lowercase().as_str() {
            "jp" => languages.push(Language::Japanese),
            "en" => languages.push(Language::English),
            "only" | "and" => {}
            _ => return (line, None),
        }
    }
    if languages.is_empty() {
        return (line, None);
    }
    languages.dedup();
    (captures.get(1).map_or("", |m| m.as_str()), Some(languages))
}

/// The lines of a "Source(s)" cell, with the languages of their tag ("(JP)", "(EN)",
/// "(EN only)", "(JP and EN)"), `None` when untagged. A tag alone on its line applies to the line
/// before.
pub fn parse_source_lines(text: &str) -> Vec<(String, Option<Vec<Language>>)> {
    let mut lines: Vec<(String, Option<Vec<Language>>)> = vec![];
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let (text, languages) = split_language_tag(line);
        if text.is_empty() {
            match lines.last_mut() {
                Some(previous) => previous.1 = languages,
                None => eprintln!("Warning: source {line:?} doesn't apply to any line"),
            }
        } else if !lines.iter().any(|(t, _)| t == text) {
            lines.push((text.to_string(), languages));
        }
    }
    lines
}

/// The sources of a printing by language. The sheet's tabs say "Unless otherwise specified, all
/// cards are in Japanese only" (and none of the PR/Birthday cards are in english): an untagged
/// line is japanese, a tagged line goes to the languages of its tag. A line only goes to a
/// language the printing is released in (`manage_id`); unreleased printings keep the tags.
pub fn promo_sources_of(
    lines: &[(String, Option<Vec<Language>>)],
    manage_id: &Localized<BTreeSet<u32>>,
) -> Localized<Vec<String>> {
    let mut sources = Localized::<Vec<String>>::default();
    for (text, languages) in lines {
        let languages = languages.clone().unwrap_or(vec![Language::Japanese]);
        for language in languages {
            if manage_id.has_value() && manage_id.value(language).is_none() {
                continue;
            }
            let values = sources.value_mut(language).get_or_insert_default();
            if !values.contains(text) {
                values.push(text.clone());
            }
        }
    }
    sources
}

// --- Unofficial names ---

/// A "Pack" line names a product when it is on most of the product's cards (recall) and mostly on
/// its cards (precision), by card number. On the sheet (2026-11), the names of products with cards
/// of their own have a precision of 1.00 and a recall of 0.62 or more; for sets of reprints
/// (collections, wafers, PR cards) the best line is another product's, with a precision of 0.48
/// or less.
const PACK_MIN_PRECISION: f32 = 0.9;
const PACK_MIN_RECALL: f32 = 0.5;

/// Where the unofficial name of a product comes from.
#[derive(Debug, Clone, PartialEq)]
pub enum UnofficialName {
    /// The official site has the name in this language.
    Official,
    /// A "Pack" line of the sheet.
    Pack(String),
    /// The title of a tab about this product only.
    TabTitle(String),
    /// A "Source(s)" line of the promos of this product (sets and collections of promo arts).
    Source(String),
    /// No name found, with the best "Pack" line and its (precision, recall) if any.
    None(Option<(String, f32, f32)>),
}

/// Sets the english unofficial names of the products the official site doesn't name in english,
/// from the "Pack" lines of the sheet (`pack_lines`: card number -> lines without language tags),
/// the tab titles, then the promo sources of the printings (see [`source_name`]). Returns where
/// each name comes from.
pub fn update_unofficial_names(
    products: &mut ProductsDatabase,
    all_cards: &CardsDatabase,
    pack_lines: &HashMap<String, BTreeSet<String>>,
    tab_titles: &[String],
) -> BTreeMap<String, UnofficialName> {
    // card number -> products, in any language
    let mut codes_of: HashMap<&str, BTreeSet<&str>> = HashMap::new();
    for illust in all_cards.values().flat_map(|cs| cs.illustrations.iter()) {
        for codes in [&illust.products.japanese, &illust.products.english]
            .into_iter()
            .flatten()
        {
            codes_of
                .entry(illust.card_number.as_str())
                .or_default()
                .extend(codes.iter().map(String::as_str));
        }
    }
    // line -> number of cards
    let mut line_counts: HashMap<&str, usize> = HashMap::new();
    for lines in pack_lines.values() {
        for line in lines {
            *line_counts.entry(line.as_str()).or_default() += 1;
        }
    }

    let mut sources = BTreeMap::new();
    for (code, product) in products.iter_mut() {
        let source = if product.name.english.is_some() {
            UnofficialName::Official
        } else {
            let numbers = codes_of
                .iter()
                .filter(|(number, codes)| {
                    codes.contains(code.as_str()) && pack_lines.contains_key(**number)
                })
                .map(|(number, _)| *number)
                .collect_vec();
            let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
            for number in &numbers {
                for line in &pack_lines[*number] {
                    *counts.entry(line.as_str()).or_default() += 1;
                }
            }
            let best = counts
                .iter()
                .map(|(line, count)| {
                    let precision = *count as f32 / line_counts[line] as f32;
                    let recall = *count as f32 / numbers.len() as f32;
                    (*line, precision, recall)
                })
                // best association, then the line for a stable choice
                .max_by(|a, b| {
                    (a.1 * a.2)
                        .total_cmp(&(b.1 * b.2))
                        .then_with(|| b.0.cmp(a.0))
                });
            match best {
                Some((line, precision, recall))
                    if precision >= PACK_MIN_PRECISION && recall >= PACK_MIN_RECALL =>
                {
                    UnofficialName::Pack(without_set_number(line).to_string())
                }
                best => match tab_title_name(code, product.category.as_deref(), tab_titles) {
                    Some(title) => UnofficialName::TabTitle(title),
                    None => match source_name(code, all_cards) {
                        Some(name) => UnofficialName::Source(name),
                        None => UnofficialName::None(best.map(|(l, p, r)| (l.to_string(), p, r))),
                    },
                },
            }
        };
        match &source {
            UnofficialName::Pack(name)
            | UnofficialName::TabTitle(name)
            | UnofficialName::Source(name) => product.unofficial_name.english = Some(name.clone()),
            // an official name replaces the translation
            UnofficialName::Official => product.unofficial_name.english = None,
            // e.g. a tab that couldn't be read: the translation stays
            UnofficialName::None(_) => {}
        }
        sources.insert(code.clone(), source);
    }
    sources
}

/// The name of a product from the "Source(s)" lines of its printings (`promo_sources`, any
/// language): the line on most of its printings and mostly on its printings (see
/// [`PACK_MIN_PRECISION`] and [`PACK_MIN_RECALL`], by printing), as the sheet writes it, e.g.
/// "hololive Official Card Game Twin Wafers" for the wafers.
fn source_name(code: &str, all_cards: &CardsDatabase) -> Option<String> {
    fn lines_of(illust: &hocg_fan_sim_assets_model::CardIllustration) -> BTreeSet<&str> {
        [
            &illust.promo_sources.japanese,
            &illust.promo_sources.english,
        ]
        .into_iter()
        .flatten()
        .flatten()
        .map(String::as_str)
        .collect()
    }
    let illustrations = all_cards
        .values()
        .flat_map(|cs| cs.illustrations.iter())
        .collect_vec();
    // line -> number of printings
    let mut line_counts: HashMap<&str, usize> = HashMap::new();
    for illust in &illustrations {
        for line in lines_of(illust) {
            *line_counts.entry(line).or_default() += 1;
        }
    }
    let in_product = illustrations
        .iter()
        .filter(|illust| {
            [&illust.products.japanese, &illust.products.english]
                .into_iter()
                .flatten()
                .any(|codes| codes.iter().any(|c| c == code))
        })
        .collect_vec();
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for illust in &in_product {
        for line in lines_of(illust) {
            *counts.entry(line).or_default() += 1;
        }
    }
    let (line, _, _) = counts
        .iter()
        .map(|(line, count)| {
            let precision = *count as f32 / line_counts[line] as f32;
            let recall = *count as f32 / in_product.len() as f32;
            (*line, precision, recall)
        })
        .filter(|(_, precision, recall)| {
            *precision >= PACK_MIN_PRECISION && *recall >= PACK_MIN_RECALL
        })
        .max_by(|a, b| {
            (a.1 * a.2)
                .total_cmp(&(b.1 * b.2))
                .then_with(|| b.0.cmp(a.0))
        })?;
    Some(line.to_string())
}

/// Drops the promo sources that only name a product of their printing: the sheet's "Source(s)"
/// of the cards of sets and collections is the name of the set ("hololive Official Card Game Twin
/// Wafers" for the wafers), that the printing's `products` already say. A source is the name of a
/// product when it is one of its names (official, short or unofficial, in any language, see
/// [`comparable_name`]). Run after [`update_unofficial_names`], that names products from these
/// sources. Returns the number of dropped sources.
pub fn drop_product_sources(all_cards: &mut CardsDatabase, products: &ProductsDatabase) -> usize {
    let names: HashMap<&str, HashSet<String>> = products
        .iter()
        .map(|(code, product)| {
            let names = [&product.name, &product.short_name, &product.unofficial_name]
                .into_iter()
                .flat_map(|name| [&name.japanese, &name.english])
                .flatten()
                .map(|name| comparable_name(name))
                .collect();
            (code.as_str(), names)
        })
        .collect();
    let mut dropped_count = 0;
    for illust in all_cards
        .values_mut()
        .flat_map(|cs| cs.illustrations.iter_mut())
    {
        let product_names = [&illust.products.japanese, &illust.products.english]
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|code| names.get(code.as_str()))
            .flatten()
            .collect::<HashSet<_>>();
        if product_names.is_empty() {
            continue;
        }
        for language in [Language::Japanese, Language::English] {
            let sources = illust.promo_sources.value_mut(language);
            if let Some(values) = sources {
                let count = values.len();
                values.retain(|source| !product_names.contains(&comparable_name(source)));
                dropped_count += count - values.len();
            }
            sources.take_if(|values| values.is_empty());
        }
    }
    dropped_count
}

/// The sheet numbers the booster sets: "Set 9 Volume Vortex" is "Volume Vortex", like the short
/// names of the official boosters.
fn without_set_number(line: &str) -> &str {
    static SET_NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^Set\s+\d+\s+").unwrap());
    match SET_NUMBER.find(line) {
        Some(m) if m.end() < line.len() => &line[m.end()..],
        _ => line,
    }
}

/// Product codes in a tab title, ranges included: "Booster hBP10 Glory Echo" is about hBP10,
/// "Start Deck hSD20-21: Kyoko & Mela" about hSD20 and hSD21.
static TAB_CODES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(h[A-Z]{2})(\d{2})(?:-(\d{2}))?\b").unwrap());

/// The products a tab of the sheet is about, from the codes in its title (see [`TAB_CODES`]).
pub fn tab_product_codes(title: &str) -> Vec<String> {
    let mut codes = vec![];
    for captures in TAB_CODES.captures_iter(title) {
        let kind = &captures[1];
        let first: u32 = captures[2].parse().unwrap_or_default();
        let last: u32 = captures
            .get(3)
            .and_then(|m| m.as_str().parse().ok())
            .unwrap_or(first);
        for number in first..=last.max(first) {
            let code = format!("{kind}{number:02}");
            if !codes.contains(&code) {
                codes.push(code);
            }
        }
    }
    codes
}

/// The product of a card row of a tab about `tab_codes`: the product of a tab about one product
/// (reprints too: an older card number in the tab of a new booster is in that booster), else
/// the one of the card number's set. `None` for tabs about no product (promos) or a card of
/// another set in a tab about several products.
pub fn row_product(card_number: &str, tab_codes: &[String]) -> Option<String> {
    match tab_codes {
        [] => None,
        [code] => Some(code.clone()),
        codes => {
            let (set, _) = card_number.split_once('-')?;
            codes.iter().find(|code| *code == set).cloned()
        }
    }
}

/// The kind of product a tab title is about: the text before its first code ("Booster" for
/// "Booster hBP10 Glory Echo", "Start Deck" for "Start Deck hSD20-21: Kyoko & Mela"). `None`
/// without a code or a kind.
pub fn tab_kind(title: &str) -> Option<String> {
    let first = TAB_CODES.find(title)?;
    let kind = title[..first.start()].split_whitespace().join(" ");
    (!kind.is_empty()).then_some(kind)
}

/// The category of a product the official site doesn't list yet (e.g. a booster before its card
/// list), from the kind of its tab (see [`tab_kind`]): the category of the products of the
/// catalog whose tabs are of the same kind ("Booster hBP09 Volume Vortex" is "boosters" on the
/// site, so "Booster hBP10 Glory Echo" is too). `None` if the kind has no category yet, or the
/// products disagree (with a warning).
pub fn tab_category(
    code: &str,
    products: &ProductsDatabase,
    tab_titles: &[String],
) -> Option<String> {
    // the kinds of the tabs of each product
    let mut kinds: HashMap<String, BTreeSet<String>> = HashMap::new();
    for title in tab_titles {
        let Some(kind) = tab_kind(title) else {
            continue;
        };
        for tab_code in tab_product_codes(title) {
            kinds.entry(tab_code).or_default().insert(kind.clone());
        }
    }
    let code_kinds = kinds.get(code)?;
    let categories = products
        .iter()
        .filter(|(other, _)| {
            *other != code
                && kinds
                    .get(*other)
                    .is_some_and(|k| !k.is_disjoint(code_kinds))
        })
        .filter_map(|(_, product)| product.category.as_deref())
        .unique()
        .collect_vec();
    match categories[..] {
        [category] => Some(category.to_owned()),
        [] => None,
        _ => {
            eprintln!(
                "Warning: product {code}: the products of its tab kind {code_kinds:?} have different categories {categories:?}, none given"
            );
            None
        }
    }
}

/// The name of a product from the title of a tab about it alone: "<kind> <code> <name>", e.g.
/// "Booster hBP10 Glory Echo". Like the short names, boosters keep the name alone ("Glory Echo"),
/// other products their kind too ("Start Deck Tokino Sora & AZKi"). Tabs about several products
/// ("Start Deck hSD20-21: Kyoko & Mela") are not used.
fn tab_title_name(code: &str, category: Option<&str>, tab_titles: &[String]) -> Option<String> {
    let clean = |text: &str| {
        text.split_whitespace()
            .join(" ")
            .trim_matches(|c: char| c == ':' || c.is_whitespace())
            .to_string()
    };
    let names = tab_titles
        .iter()
        .filter(|title| {
            let codes = TAB_CODES.find_iter(title).map(|m| m.as_str()).collect_vec();
            codes == [code]
        })
        .filter_map(|title| {
            let (kind, name) = title.split_once(code)?;
            let (kind, name) = (clean(kind), clean(name));
            if category == Some(BOOSTERS_CATEGORY) || kind.is_empty() {
                Some(name)
            } else if name.is_empty() {
                Some(kind)
            } else {
                Some(format!("{kind} {name}"))
            }
        })
        .filter(|name| !name.is_empty())
        .collect_vec();
    // only when the tab is the only one about it
    match &names[..] {
        [name] => Some(name.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_tab_category() {
        use super::{tab_category, tab_kind};
        use hocg_fan_sim_assets_model::{Product, ProductsDatabase};
        assert_eq!(
            tab_kind("Booster hBP10 Glory Echo").as_deref(),
            Some("Booster")
        );
        assert_eq!(
            tab_kind("Start Deck hSD20-21: Kyoko & Mela").as_deref(),
            Some("Start Deck")
        );
        assert_eq!(tab_kind("hBP10 Glory Echo"), None);
        assert_eq!(tab_kind("Promo Art 2026"), None);

        let product = |category: Option<&str>| Product {
            category: category.map(str::to_owned),
            ..Default::default()
        };
        let products: ProductsDatabase = [
            ("hBP08", product(Some("boosters"))),
            ("hBP09", product(Some("boosters"))),
            ("hBP10", product(None)),
            ("hEB01", product(Some("boosters"))),
            ("hSD20", product(Some("decks"))),
            ("hCO01", product(Some("accessories"))),
            ("hCO02", product(Some("collections"))),
        ]
        .map(|(code, product)| (code.to_string(), product))
        .into();
        let titles = [
            "Booster hBP08 Bouncer Bound",
            "Booster hBP09 Volume Vortex",
            "Booster hBP10 Glory Echo",
            "Extra Booster hEB01 Summer hologram",
            "Extra Booster hEB02 Winter",
            "Start Deck hSD20-21: Kyoko & Mela",
            "Collection hCO01-03",
            "Gift hGF01",
            "Master Sheet",
        ]
        .map(String::from);
        assert_eq!(
            tab_category("hBP10", &products, &titles).as_deref(),
            Some("boosters")
        );
        assert_eq!(
            tab_category("hEB02", &products, &titles).as_deref(),
            Some("boosters")
        );
        assert_eq!(
            tab_category("hSD21", &products, &titles).as_deref(),
            Some("decks")
        );
        // the products of the kind disagree, a kind without category yet, no tab
        assert_eq!(tab_category("hCO03", &products, &titles), None);
        assert_eq!(tab_category("hGF01", &products, &titles), None);
        assert_eq!(tab_category("hBP11", &products, &titles), None);
    }

    #[test]
    fn test_tab_product_codes() {
        use super::{row_product, tab_product_codes};
        assert_eq!(tab_product_codes("Booster hBP10 Glory Echo"), ["hBP10"]);
        assert_eq!(
            tab_product_codes("Start Deck hSD20-21: Kyoko & Mela"),
            ["hSD20", "hSD21"]
        );
        assert_eq!(
            tab_product_codes("Start Deck hSD05-07: Hajime, Iroha, & Flare"),
            ["hSD05", "hSD06", "hSD07"]
        );
        assert!(tab_product_codes("Promo Art 2026").is_empty());
        assert!(tab_product_codes("PR/Birthday Cards").is_empty());
        assert!(tab_product_codes("Master Sheet").is_empty());

        let codes = |title: &str| tab_product_codes(title);
        // a reprint in the tab of a booster is in that booster
        assert_eq!(
            row_product("hBP01-032", &codes("Booster hBP10 Glory Echo")).as_deref(),
            Some("hBP10")
        );
        // a tab about several products: the card number's set
        assert_eq!(
            row_product("hSD21-002", &codes("Start Deck hSD20-21: Kyoko & Mela")).as_deref(),
            Some("hSD21")
        );
        assert_eq!(
            row_product("hBP01-001", &codes("Start Deck hSD20-21: Kyoko & Mela")),
            None
        );
        assert_eq!(row_product("hBP10-001", &codes("Promo Art 2026")), None);
    }

    #[test]
    fn test_listing_page() {
        use super::listing_page;
        let listing = [
            (
                "スタートデッキセット-2025 ホロナツパラダイスver.-",
                "jp/startdeckset-2025summer/",
            ),
            ("ブースターパック「エリートスパーク」", "jp/elite-spark/"),
            (
                "hololive OFFICIAL CARD GAME Start Deck – Red Nakiri Ayame",
                "en/start-deck-red-nakiri-ayame/",
            ),
            ("Start Cheer Set", "en/start-cheer-set/"),
            ("Start Cheer Set 2", "en/start-cheer-set-2/"),
            ("Booster Pack – Elite Spark", "en/elite-spark/"),
            ("Booster Pack – Elite Spark", "en/elite-spark-again/"),
        ]
        .map(|(name, url)| (name.to_string(), url.to_string()));
        // a note in the name, or the name of the game added
        assert_eq!(
            listing_page(
                "【イベント物販限定商品】スタートデッキセット-2025 ホロナツパラダイスver.",
                &listing
            ),
            Some("jp/startdeckset-2025summer/")
        );
        assert_eq!(
            listing_page("ブースターパック「エリートスパーク」", &listing),
            Some("jp/elite-spark/")
        );
        assert_eq!(
            listing_page("Start Deck – Red Nakiri Ayame", &listing),
            Some("en/start-deck-red-nakiri-ayame/")
        );
        // the closest
        assert_eq!(
            listing_page("Start Cheer Set", &listing),
            Some("en/start-cheer-set/")
        );
        // as close: none
        assert_eq!(listing_page("Booster Pack – Elite Spark", &listing), None);
        assert_eq!(
            listing_page("Start Deck – Blue Nekomata Okayu", &listing),
            None
        );
        assert_eq!(listing_page("【】", &listing), None);
    }

    #[test]
    fn test_fix_product_urls() {
        use super::fix_product_urls;
        use hocg_fan_sim_assets_model::{Language, Localized, Product, ProductsDatabase};
        let product = |name: &str, url: &str| Product {
            name: Localized::new(Language::Japanese, name.into()),
            url: Localized::new(Language::Japanese, url.into()),
            ..Default::default()
        };
        let mut products: ProductsDatabase = [
            (
                "hSD2025summer",
                product("スタートデッキセット-2025", "dead/holonatsu/"),
            ),
            ("hBP01", product("ブースターパック", "listed/hBP01/")),
            ("hCS01", product("セット", "dead/hCS01/")),
            ("hWF01", product("ウエハース", "alive/elsewhere/")),
            ("hBP02", product("ブースターパック2", "unreachable/hBP02/")),
        ]
        .map(|(code, product)| (code.to_string(), product))
        .into();
        let listing = [
            (
                "スタートデッキセット-2025 ver.",
                "listed/startdeckset-2025summer/",
            ),
            ("ブースターパック", "listed/hBP01/"),
        ]
        .map(|(name, url)| (name.to_string(), url.to_string()));
        let is_dead = |url: &str| {
            assert!(!url.starts_with("listed/"), "listed urls are not checked");
            match url.split('/').next() {
                Some("dead") => Some(true),
                Some("alive") => Some(false),
                _ => None,
            }
        };
        assert_eq!(
            fix_product_urls(&mut products, &listing, Language::Japanese, is_dead),
            2
        );
        let url = |code: &str| products[code].url.japanese.as_deref();
        // replaced by the page of the listing
        assert_eq!(
            url("hSD2025summer"),
            Some("listed/startdeckset-2025summer/")
        );
        // dead and not listed: removed
        assert_eq!(url("hCS01"), None);
        // kept: listed, alive, or can't be checked
        assert_eq!(url("hBP01"), Some("listed/hBP01/"));
        assert_eq!(url("hWF01"), Some("alive/elsewhere/"));
        assert_eq!(url("hBP02"), Some("unreachable/hBP02/"));
    }

    use hocg_fan_sim_assets_model::{CardIllustration, Localized, Product};

    use super::*;

    #[test]
    fn test_expansion_kind() {
        // every code of the JP and EN card search and card pages (2026-11)
        let products = [
            "hBP01",
            "hBP02",
            "hBP03",
            "hBP04",
            "hBP05",
            "hBP06",
            "hBP07",
            "hBP08",
            "hBP09",
            "hCO01",
            "hCO02",
            "hCO03",
            "hCS01",
            "hCS02",
            "hEB01",
            "hPC01",
            "hPR",
            "hSD01",
            "hSD02",
            "hSD03",
            "hSD04",
            "hSD05",
            "hSD06",
            "hSD07",
            "hSD08",
            "hSD09",
            "hSD10",
            "hSD11",
            "hSD12",
            "hSD13",
            "hSD14",
            "hSD15",
            "hSD16",
            "hSD17",
            "hSD18",
            "hSD19",
            "hSD20",
            "hSD21",
            "hSD2025summer",
            "hWF01",
            "hYS01",
        ];
        for code in products {
            assert_eq!(expansion_kind(code), ExpansionKind::Product, "{code}");
        }
        assert_eq!(expansion_kind("sele07"), ExpansionKind::CardPool);
        assert_eq!(expansion_kind("sele12"), ExpansionKind::CardPool);
        // neither: logged, kept
        for code in [
            "", "sele", "hbp01", "HBP01", "hBP1", "hBP001", "hBP01 ", "hBPX01", "abc",
        ] {
            assert_eq!(expansion_kind(code), ExpansionKind::Unknown, "{code:?}");
        }
    }

    fn entry(name: &str, code: Option<&str>, date: Option<&str>, category: &str) -> ProductEntry {
        ProductEntry {
            name: name.into(),
            expansion: code.map(Into::into),
            release_date: date.map(Into::into),
            category: Some(category.into()),
            url: None,
        }
    }

    #[test]
    fn test_update_catalog() {
        let mut products = ProductsDatabase::new();
        let jp = update_catalog(
            &mut products,
            &[entry(
                "ブースターパック「ブルーミングレディアンス」",
                Some("hBP01"),
                Some("2024-09-20"),
                "boosters",
            )],
            Language::Japanese,
        );
        assert_eq!(
            jp["ブースターパック「ブルーミングレディアンス」"],
            ListedProduct::Product("hBP01".into())
        );
        let en = update_catalog(
            &mut products,
            &[
                entry(
                    "Booster Pack – Blooming Radiance",
                    Some("hBP01"),
                    Some("2025-07-11"),
                    "boosters",
                ),
                entry(
                    "Selection Cup - Diva Fever",
                    Some("sele07"),
                    Some("2026-11-20"),
                    "accessories",
                ),
                entry("PROMO CARDS", Some("hPR"), None, "pr"),
                entry("Something new", None, Some("2027-01-01"), "boosters"),
            ],
            Language::English,
        );
        assert_eq!(en["Selection Cup - Diva Fever"], ListedProduct::CardPool);
        assert_eq!(en["Something new"], ListedProduct::Unknown);
        assert_eq!(en["PROMO CARDS"], ListedProduct::Product("hPR".into()));
        // card pools are not in the catalog
        assert_eq!(products.keys().collect_vec(), ["hBP01", "hPR"]);
        assert_eq!(
            products["hBP01"],
            Product {
                name: Localized::both(
                    "ブースターパック「ブルーミングレディアンス」".into(),
                    "Booster Pack – Blooming Radiance".into()
                ),
                release_date: Localized::both("2024-09-20".into(), "2025-07-11".into()),
                category: Some("boosters".into()),
                url: Localized::default(),
                ..Default::default()
            }
        );

        // a page without the date keeps it, another category is not taken
        update_catalog(
            &mut products,
            &[entry(
                "Booster Pack – Blooming Radiance",
                Some("hBP01"),
                None,
                "decks",
            )],
            Language::English,
        );
        assert_eq!(
            products["hBP01"].release_date.english.as_deref(),
            Some("2025-07-11")
        );
        assert_eq!(products["hBP01"].category.as_deref(), Some("boosters"));
    }

    #[test]
    fn test_needs_details() {
        let mut products = ProductsDatabase::new();
        update_catalog(
            &mut products,
            &[
                entry("Booster", Some("hBP01"), Some("2025-07-11"), "boosters"),
                entry("PROMO CARDS", Some("hPR"), None, "pr"),
            ],
            Language::English,
        );
        let names = catalog_names(&products, Language::English);
        assert!(!needs_details(
            &products,
            &names,
            "Booster",
            Language::English
        ));
        // undated, or unknown
        assert!(needs_details(
            &products,
            &names,
            "PROMO CARDS",
            Language::English
        ));
        assert!(needs_details(
            &products,
            &names,
            "New Booster",
            Language::English
        ));
        // not in this language yet
        let names = catalog_names(&products, Language::Japanese);
        assert!(needs_details(
            &products,
            &names,
            "Booster",
            Language::Japanese
        ));
    }

    #[test]
    fn test_update_printings() {
        let mut all_cards = CardsDatabase::new();
        let card = all_cards.entry("hBP01-021".into()).or_default();
        card.illustrations.push(CardIllustration {
            card_number: "hBP01-021".into(),
            manage_id: Localized::en([3, 4].into()),
            products: Localized::en(vec!["hPR".into()]),
            ..Default::default()
        });
        let resolved = HashMap::from([
            (
                "Start Deck".to_string(),
                ListedProduct::Product("hSD01".into()),
            ),
            (
                "Booster".to_string(),
                ListedProduct::Product("hBP01".into()),
            ),
            ("Selection Cup".to_string(), ListedProduct::CardPool),
            ("Broken".to_string(), ListedProduct::Unknown),
        ]);
        let products = |all_cards: &CardsDatabase| {
            all_cards["hBP01-021"].illustrations[0]
                .products
                .english
                .clone()
        };

        // id 4 not listed: add to the stored codes
        let listed = BTreeMap::from([(
            3,
            vec![
                "Start Deck".into(),
                "Booster".into(),
                "Selection Cup".into(),
            ],
        )]);
        let (updated, unknown) =
            update_printings(&mut all_cards, &listed, &resolved, Language::English);
        assert_eq!((updated, unknown.len()), (1, 0));
        assert_eq!(
            products(&all_cards),
            Some(vec!["hPR".into(), "hSD01".into(), "hBP01".into()])
        );

        // all listed: replace, in page order, without the card pool
        let listed = BTreeMap::from([
            (3, vec!["Booster".into(), "Selection Cup".into()]),
            (4, vec!["Start Deck".into()]),
        ]);
        update_printings(&mut all_cards, &listed, &resolved, Language::English);
        assert_eq!(
            products(&all_cards),
            Some(vec!["hBP01".into(), "hSD01".into()])
        );

        // an unknown name: keep the stored codes
        let listed = BTreeMap::from([(3, vec!["Broken".into()]), (4, vec![])]);
        let (updated, unknown) =
            update_printings(&mut all_cards, &listed, &resolved, Language::English);
        assert_eq!(
            (updated, unknown.into_iter().collect_vec()),
            (0, vec!["Broken".into()])
        );
        assert_eq!(
            products(&all_cards),
            Some(vec!["hBP01".into(), "hSD01".into()])
        );

        // nothing listed (layout change): keep
        let listed = BTreeMap::from([(3, vec![]), (4, vec![])]);
        update_printings(&mut all_cards, &listed, &resolved, Language::English);
        assert_eq!(
            products(&all_cards),
            Some(vec!["hBP01".into(), "hSD01".into()])
        );
        assert_eq!(
            all_cards["hBP01-021"].illustrations[0].products.japanese,
            None
        );
    }

    /// The cards of a card list page that couldn't be read are not listed: they keep their
    /// products, and the products are not removed.
    #[test]
    fn test_unread_pages_keep_their_products() {
        let mut all_cards = CardsDatabase::new();
        for (number, id, code) in [("hBP09-001", 1, "hBP09"), ("hBP08-001", 2, "hBP08")] {
            all_cards
                .entry(number.into())
                .or_default()
                .illustrations
                .push(CardIllustration {
                    card_number: number.into(),
                    manage_id: Localized::jp([id].into()),
                    products: Localized::jp(vec![code.into()]),
                    ..Default::default()
                });
        }
        let mut products: ProductsDatabase = ["hBP09", "hBP08"]
            .into_iter()
            .map(|code| (code.to_string(), Product::default()))
            .collect();
        // only the page of hBP09-001 was read
        let listed = BTreeMap::from([(1, vec!["Volume Vortex".to_string()])]);
        let resolved = HashMap::from([(
            "Volume Vortex".to_string(),
            ListedProduct::Product("hBP09".into()),
        )]);
        update_printings(&mut all_cards, &listed, &resolved, Language::Japanese);
        assert_eq!(
            all_cards["hBP08-001"].illustrations[0].products.japanese,
            Some(vec!["hBP08".to_string()])
        );
        assert!(remove_unused_products(&mut products, &all_cards).is_empty());
        assert_eq!(products.len(), 2);
    }

    #[test]
    fn test_remove_unused_products() {
        let mut all_cards = CardsDatabase::new();
        all_cards
            .entry("hBP01-021".into())
            .or_default()
            .illustrations
            .push(CardIllustration {
                products: Localized::both(vec!["hBP01".into()], vec!["hSD01".into()]),
                ..Default::default()
            });
        let mut products: ProductsDatabase = ["hBP01", "hSD01", "hBP02"]
            .into_iter()
            .map(|code| (code.to_string(), Product::default()))
            .collect();
        assert_eq!(remove_unused_products(&mut products, &all_cards), ["hBP02"]);
        assert_eq!(products.keys().collect_vec(), ["hBP01", "hSD01"]);
    }

    /// (code, category, japanese name, short, english name, short) of the official sites (2026-11)
    #[allow(clippy::type_complexity)]
    const NAMES: &[(
        &str,
        &str,
        Option<&str>,
        Option<&str>,
        Option<&str>,
        Option<&str>,
    )] = &[
        (
            "hBP01",
            "boosters",
            Some("ブースターパック「ブルーミングレディアンス」"),
            Some("ブルーミングレディアンス"),
            Some("Booster Pack – Blooming Radiance"),
            Some("Blooming Radiance"),
        ),
        (
            "hBP02",
            "boosters",
            Some("ブースターパック「クインテットスペクトラム」"),
            Some("クインテットスペクトラム"),
            Some("Booster Pack – Quintet Spectrum"),
            Some("Quintet Spectrum"),
        ),
        (
            "hBP03",
            "boosters",
            Some("ブースターパック「エリートスパーク」"),
            Some("エリートスパーク"),
            Some("Booster Pack – Elite Spark"),
            Some("Elite Spark"),
        ),
        (
            "hBP04",
            "boosters",
            Some("ブースターパック「キュリアスユニバース」"),
            Some("キュリアスユニバース"),
            Some("Booster Pack – Curious Universe"),
            Some("Curious Universe"),
        ),
        (
            "hBP05",
            "boosters",
            Some("ブースターパック「エンチャントレガリア」"),
            Some("エンチャントレガリア"),
            Some("Booster Pack – Enchant Regalia"),
            Some("Enchant Regalia"),
        ),
        (
            "hBP06",
            "boosters",
            Some("ブースターパック「アヤカシヴァーミリオン」"),
            Some("アヤカシヴァーミリオン"),
            Some("Booster Pack – Ayakashi Vermilion"),
            Some("Ayakashi Vermilion"),
        ),
        (
            "hBP07",
            "boosters",
            Some("ブースターパック「ディーヴァフィーバー」"),
            Some("ディーヴァフィーバー"),
            Some("Booster Pack - Diva Fever"),
            Some("Diva Fever"),
        ),
        (
            "hBP08",
            "boosters",
            Some("ブースターパック バウンサーバウンド"),
            Some("バウンサーバウンド"),
            None,
            None,
        ),
        (
            "hBP09",
            "boosters",
            Some("ブースターパック「ボリュームヴォルテックス」"),
            Some("ボリュームヴォルテックス"),
            None,
            None,
        ),
        (
            "hCO01",
            "accessories",
            Some("オフィシャルホロカコレクション-2025ライブセット-"),
            None,
            None,
            None,
        ),
        (
            "hCO02",
            "accessories",
            Some("エクストリーマーカップ25-26 Final オフィシャルホロカコレクションA"),
            None,
            None,
            None,
        ),
        (
            "hCO03",
            "accessories",
            Some("エクストリーマーカップ25-26 Final オフィシャルホロカコレクションB"),
            None,
            None,
            None,
        ),
        (
            "hCS01",
            "accessories",
            Some(
                "【hololive production OFFICIAL SHOP限定】hololive OFFICIAL CARD GAME 1st Anniversary Celebration Set",
            ),
            Some("hololive OFFICIAL CARD GAME 1st Anniversary Celebration Set"),
            None,
            None,
        ),
        (
            "hCS02",
            "accessories",
            Some("2nd Anniversary Celebration Set"),
            None,
            None,
            None,
        ),
        (
            "hEB01",
            "boosters",
            Some("エクストラブースター サマー・ホログラム"),
            Some("サマー・ホログラム"),
            Some("Extra Booster – Summer hologram"),
            Some("Summer hologram"),
        ),
        (
            "hPC01",
            "accessories",
            Some(
                "【イベント物販／hololive production OFFICIAL SHOP限定商品】オフィシャルホロカコレクション-PCセット-",
            ),
            Some("オフィシャルホロカコレクション-PCセット-"),
            None,
            None,
        ),
        (
            "hPR",
            "pr",
            Some("PRカード"),
            None,
            Some("PROMO CARDS"),
            None,
        ),
        (
            "hSD01",
            "decks",
            Some("スタートデッキ「ときのそら＆AZKi」"),
            Some("スタートデッキ ときのそら＆AZKi"),
            Some("Start Deck – Tokino Sora & AZKi"),
            Some("Start Deck Tokino Sora & AZKi"),
        ),
        (
            "hSD02",
            "decks",
            Some("スタートデッキ 赤 百鬼あやめ"),
            None,
            Some("Start Deck – Red Nakiri Ayame"),
            Some("Start Deck Red Nakiri Ayame"),
        ),
        (
            "hSD03",
            "decks",
            Some("スタートデッキ 青 猫又おかゆ"),
            None,
            Some("Start Deck – Blue Nekomata Okayu"),
            Some("Start Deck Blue Nekomata Okayu"),
        ),
        (
            "hSD04",
            "decks",
            Some("スタートデッキ 紫 癒月ちょこ"),
            None,
            Some("Start Deck – Purple Yuzuki Choco"),
            Some("Start Deck Purple Yuzuki Choco"),
        ),
        (
            "hSD05",
            "decks",
            Some("スタートデッキ 白 轟はじめ"),
            None,
            Some("Start Deck – White Todoroki Hajime"),
            Some("Start Deck White Todoroki Hajime"),
        ),
        (
            "hSD06",
            "decks",
            Some("スタートデッキ 緑 風真いろは"),
            None,
            Some("Start Deck – Green Kazama Iroha"),
            Some("Start Deck Green Kazama Iroha"),
        ),
        (
            "hSD07",
            "decks",
            Some("スタートデッキ 黄 不知火フレア"),
            None,
            Some("Start Deck – Yellow Shiranui Flare"),
            Some("Start Deck Yellow Shiranui Flare"),
        ),
        (
            "hSD08",
            "decks",
            Some("スタートデッキ 白 天音かなた"),
            None,
            None,
            None,
        ),
        (
            "hSD09",
            "decks",
            Some("スタートデッキ 赤 宝鐘マリン"),
            None,
            None,
            None,
        ),
        (
            "hSD10",
            "decks",
            Some("スタートデッキ FLOW GLOW 推し 輪堂千速"),
            None,
            None,
            None,
        ),
        (
            "hSD11",
            "decks",
            Some("スタートデッキ FLOW GLOW 推し 虎金妃笑虎"),
            None,
            None,
            None,
        ),
        (
            "hSD12",
            "decks",
            Some("スタートデッキ 推し Advent"),
            None,
            Some("Start Deck – Oshi Advent"),
            Some("Start Deck Oshi Advent"),
        ),
        (
            "hSD13",
            "decks",
            Some("スタートデッキ 推し Justice"),
            None,
            Some("Start Deck – Oshi Justice"),
            Some("Start Deck Oshi Justice"),
        ),
        (
            "hSD14",
            "decks",
            Some("ライブスタートデッキ 白上フブキ"),
            None,
            Some("Live Start Deck – Shirakami Fubuki"),
            Some("Live Start Deck Shirakami Fubuki"),
        ),
        (
            "hSD15",
            "decks",
            Some("ライブスタートデッキ 儒烏風亭らでん"),
            None,
            Some("Live Start Deck – Juufuutei Raden"),
            Some("Live Start Deck Juufuutei Raden"),
        ),
        (
            "hSD16",
            "decks",
            Some("ライブスタートデッキ さくらみこ"),
            None,
            Some("Live Start Deck – Sakura Miko"),
            Some("Live Start Deck Sakura Miko"),
        ),
        (
            "hSD17",
            "decks",
            Some("ライブスタートデッキ 星街すいせい"),
            None,
            Some("Live Start Deck – Hoshimachi Suisei"),
            Some("Live Start Deck Hoshimachi Suisei"),
        ),
        (
            "hSD18",
            "decks",
            Some("ライブスタートデッキ 森カリオペ"),
            None,
            Some("Live Start Deck – Mori Calliope"),
            Some("Live Start Deck Mori Calliope"),
        ),
        (
            "hSD19",
            "decks",
            Some("ライブスタートデッキ 大空スバル"),
            None,
            Some("Live Start Deck – Oozora Subaru"),
            Some("Live Start Deck Oozora Subaru"),
        ),
        (
            "hSD20",
            "decks",
            Some("スタートデッキ アソビ★まわり隊！推し 百灯キョーコ"),
            None,
            None,
            None,
        ),
        (
            "hSD2025summer",
            "decks",
            Some("【イベント物販限定商品】スタートデッキセット-2025 ホロナツパラダイスver."),
            Some("スタートデッキセット-2025 ホロナツパラダイスver."),
            None,
            None,
        ),
        (
            "hSD21",
            "decks",
            Some("スタートデッキ アソビ★まわり隊！推し 熱千めら"),
            None,
            None,
            None,
        ),
        (
            "hWF01",
            "accessories",
            Some("hololive OFFICIAL CARD GAME ツインウエハース"),
            None,
            None,
            None,
        ),
        (
            "hYS01",
            "accessories",
            Some("スタートエールセット"),
            None,
            Some("Start Cheer Set"),
            None,
        ),
    ];

    #[test]
    fn test_short_names_of_all_products() {
        let mut products: ProductsDatabase = NAMES
            .iter()
            .map(|(code, category, jp, _, en, _)| {
                let product = Product {
                    name: Localized {
                        japanese: jp.map(Into::into),
                        english: en.map(Into::into),
                    },
                    category: Some(category.to_string()),
                    ..Default::default()
                };
                (code.to_string(), product)
            })
            .collect();
        update_short_names(&mut products);
        for (code, _, _, jp_short, _, en_short) in NAMES {
            let product = &products[*code];
            assert_eq!(
                product.short_name.japanese.as_deref(),
                *jp_short,
                "{code} jp"
            );
            assert_eq!(
                product.short_name.english.as_deref(),
                *en_short,
                "{code} en"
            );
        }
        assert_eq!(NAMES.len(), 41);
    }

    #[test]
    fn test_fix_promos_in_boosters() {
        use hocg_fan_sim_assets_model::{CardIllustration, CardRarity};
        let printing = |rarity: CardRarity, jp: &[&str], en: &[&str]| CardIllustration {
            card_number: "hBP07-100".into(),
            rarity,
            products: Localized {
                japanese: (!jp.is_empty()).then(|| jp.iter().map(|c| c.to_string()).collect()),
                english: (!en.is_empty()).then(|| en.iter().map(|c| c.to_string()).collect()),
            },
            ..Default::default()
        };
        let mut all_cards = CardsDatabase::new();
        all_cards
            .entry("hBP07-100".into())
            .or_default()
            .illustrations = vec![
            // the english page lists the booster
            printing(CardRarity::Promo, &["hPR"], &["hBP07"]),
            // nothing else to go by
            printing(CardRarity::Promo, &[], &["hBP07"]),
            // not a promo
            printing(CardRarity::Uncommon, &["hBP07"], &["hBP07"]),
        ];
        let product = |category: &str| Product {
            category: Some(category.into()),
            ..Default::default()
        };
        let products: ProductsDatabase = [("hBP07", product("boosters")), ("hPR", product("pr"))]
            .map(|(code, product)| (code.to_string(), product))
            .into();

        assert_eq!(fix_promos_in_boosters(&mut all_cards, &products), 1);
        let products_of =
            |index: usize| all_cards["hBP07-100"].illustrations[index].products.clone();
        assert_eq!(
            products_of(0),
            Localized::both(vec!["hPR".to_string()], vec!["hPR".to_string()])
        );
        assert_eq!(products_of(1), Localized::en(vec!["hBP07".to_string()]));
        assert_eq!(
            products_of(2),
            Localized::both(vec!["hBP07".to_string()], vec!["hBP07".to_string()])
        );
    }

    #[test]
    fn test_short_names_in_line_with_the_sheet() {
        let product = |name: &str, sheet: &str| Product {
            name: Localized::jp(name.into()),
            category: Some("accessories".into()),
            unofficial_name: Localized::en(sheet.into()),
            ..Default::default()
        };
        let mut products: ProductsDatabase = [
            (
                "hCS01",
                product(
                    "【hololive production OFFICIAL SHOP限定】hololive OFFICIAL CARD GAME 1st Anniversary Celebration Set",
                    "1st Anniversary Celebration Set",
                ),
            ),
            (
                "hWF01",
                product(
                    "hololive OFFICIAL CARD GAME ツインウエハース",
                    "hololive Official Card Game Twin Wafers",
                ),
            ),
            (
                "hCS02",
                product("2nd Anniversary Celebration Set", "2nd Anniversary Celebration Set"),
            ),
        ]
        .map(|(code, product)| (code.to_string(), product))
        .into();
        update_short_names(&mut products);
        assert_eq!(
            products["hCS01"].short_name,
            Localized::jp("1st Anniversary Celebration Set".into())
        );
        assert_eq!(products["hWF01"].short_name, Localized::default());
        assert_eq!(products["hCS02"].short_name, Localized::default());
    }

    #[test]
    fn test_short_name_keeps_the_game() {
        assert_eq!(
            short_name(
                "hololive OFFICIAL CARD GAME ツインウエハース",
                Some("accessories")
            ),
            None
        );
        assert_eq!(
            short_name(
                "【hololive production OFFICIAL SHOP限定】hololive OFFICIAL CARD GAME 1st Anniversary Celebration Set",
                Some("accessories")
            )
            .as_deref(),
            Some("hololive OFFICIAL CARD GAME 1st Anniversary Celebration Set")
        );
    }

    #[test]
    fn test_short_name_rules() {
        let boosters = Some("boosters");
        let decks = Some("decks");
        // boosters: the set name alone
        assert_eq!(
            short_name("ブースターパック「ボリュームヴォルテックス」", boosters).as_deref(),
            Some("ボリュームヴォルテックス")
        );
        assert_eq!(
            short_name("Booster Pack – Blooming Radiance", boosters).as_deref(),
            Some("Blooming Radiance")
        );
        assert_eq!(
            short_name("エクストラブースター サマー・ホログラム", boosters).as_deref(),
            Some("サマー・ホログラム")
        );
        assert_eq!(short_name("Booster", boosters), None);
        // others keep their kind
        assert_eq!(
            short_name("スタートデッキ「ときのそら＆AZKi」", decks).as_deref(),
            Some("スタートデッキ ときのそら＆AZKi")
        );
        assert_eq!(
            short_name("Live Start Deck – Mori Calliope", decks).as_deref(),
            Some("Live Start Deck Mori Calliope")
        );
        assert_eq!(short_name("スタートデッキ 赤 百鬼あやめ", decks), None);
        // the 【】 note goes, dashes without spaces are part of the name
        assert_eq!(
            short_name("【イベント物販限定商品】スタートデッキセット-2025", decks).as_deref(),
            Some("スタートデッキセット-2025")
        );
        assert_eq!(
            short_name(
                "オフィシャルホロカコレクション-2025ライブセット-",
                Some("accessories")
            ),
            None
        );
        assert_eq!(short_name("PRカード", Some("pr")), None);
        assert_eq!(short_name("Start Cheer Set", None), None);
    }

    #[test]
    fn test_split_language_tag() {
        let jp = Some(vec![Language::Japanese]);
        let en = Some(vec![Language::English]);
        assert_eq!(
            split_language_tag("Basic PR Pack vol 1 (JP)"),
            ("Basic PR Pack vol 1", jp.clone())
        );
        assert_eq!(
            split_language_tag("Bloom Cup (EN only)"),
            ("Bloom Cup", en.clone())
        );
        assert_eq!(
            split_language_tag("Trio Challenge (JP and EN)"),
            (
                "Trio Challenge",
                Some(vec![Language::Japanese, Language::English])
            )
        );
        assert_eq!(split_language_tag("(EN only)"), ("", en));
        // not a language tag
        assert_eq!(
            split_language_tag("Some cup (JP format)"),
            ("Some cup (JP format)", None)
        );
        assert_eq!(
            split_language_tag("Mikodanye PR Pack (cards are not foiled)").1,
            None
        );
        assert_eq!(
            split_language_tag("Set 1 Blooming Radiance"),
            ("Set 1 Blooming Radiance", None)
        );
    }

    #[test]
    fn test_parse_promo_sources() {
        let jp = Some(vec![Language::Japanese]);
        let en = Some(vec![Language::English]);
        // from the sheet
        let lines = parse_source_lines(
            "Basic PR Pack vol 1 (JP)\n\nMonthly Tournament Pack Vol.01 2025 (EN)\nBloom Cup and Bloom Cup DX top results",
        );
        assert_eq!(
            lines,
            [
                ("Basic PR Pack vol 1".to_string(), jp.clone()),
                (
                    "Monthly Tournament Pack Vol.01 2025".to_string(),
                    en.clone()
                ),
                ("Bloom Cup and Bloom Cup DX top results".to_string(), None),
            ]
        );
        // a tag alone applies to the line before
        assert_eq!(
            parse_source_lines("Various event giveaways\n(EN only)"),
            [("Various event giveaways".to_string(), en.clone())]
        );
        assert!(parse_source_lines(" \n").is_empty());

        let both: Localized<BTreeSet<u32>> = Localized::both([1].into(), [2].into());
        let jp_only: Localized<BTreeSet<u32>> = Localized::jp([1].into());
        let en_only: Localized<BTreeSet<u32>> = Localized::en([2].into());
        // untagged lines are japanese ("Unless otherwise specified, all cards are in Japanese only")
        assert_eq!(
            promo_sources_of(&lines, &both),
            Localized::both(
                vec![
                    "Basic PR Pack vol 1".to_string(),
                    "Bloom Cup and Bloom Cup DX top results".to_string()
                ],
                vec!["Monthly Tournament Pack Vol.01 2025".to_string()],
            )
        );
        // only the languages the printing is released in
        assert_eq!(
            promo_sources_of(&lines, &jp_only),
            Localized::jp(vec![
                "Basic PR Pack vol 1".to_string(),
                "Bloom Cup and Bloom Cup DX top results".to_string()
            ])
        );
        assert_eq!(
            promo_sources_of(&lines, &en_only),
            Localized::en(vec!["Monthly Tournament Pack Vol.01 2025".to_string()])
        );
        // unreleased: the tags
        assert_eq!(
            promo_sources_of(&lines, &Localized::default()),
            promo_sources_of(&lines, &both)
        );
        // "(JP and EN)"
        let lines = parse_source_lines("Trio Challenge (JP and EN)");
        assert_eq!(
            promo_sources_of(&lines, &both),
            Localized::both(
                vec!["Trio Challenge".to_string()],
                vec!["Trio Challenge".to_string()]
            )
        );
    }

    #[test]
    fn test_update_unofficial_names() {
        let mut all_cards = CardsDatabase::new();
        let mut printing = |number: &str, codes: &[&str]| {
            all_cards
                .entry(number.into())
                .or_default()
                .illustrations
                .push(CardIllustration {
                    card_number: number.into(),
                    products: Localized::jp(codes.iter().map(|c| c.to_string()).collect()),
                    ..Default::default()
                });
        };
        // a set with its own cards, a reprint collection, an official english name
        for n in 1..=4 {
            printing(&format!("hBP09-00{n}"), &["hBP09"]);
        }
        for n in 2..=5 {
            printing(&format!("hBP01-00{n}"), &["hBP01"]);
        }
        printing("hBP01-001", &["hBP01", "hCO01"]);
        printing("hBP09-005", &["hBP09", "hCO01"]);
        let pack_lines: HashMap<String, BTreeSet<String>> = [
            ("hBP09-001", "Set 9 Volume Vortex"),
            ("hBP09-002", "Set 9 Volume Vortex"),
            ("hBP09-003", "Set 9 Volume Vortex"),
            ("hBP09-004", "Set 9 Volume Vortex"),
            ("hBP09-005", "Set 9 Volume Vortex"),
            ("hBP01-001", "Set 1 Blooming Radiance"),
            ("hBP01-002", "Set 1 Blooming Radiance"),
            ("hBP01-003", "Set 1 Blooming Radiance"),
            ("hBP01-004", "Set 1 Blooming Radiance"),
            ("hBP01-005", "Set 1 Blooming Radiance"),
        ]
        .into_iter()
        .map(|(n, l)| (n.to_string(), BTreeSet::from([l.to_string()])))
        .collect();
        let mut products: ProductsDatabase = ["hBP09", "hCO01", "hBP01", "hBP10"]
            .into_iter()
            .map(|c| {
                let product = Product {
                    category: c.starts_with("hBP").then(|| "boosters".to_string()),
                    ..Default::default()
                };
                (c.to_string(), product)
            })
            .collect();
        products.get_mut("hBP01").unwrap().name =
            Localized::en("Booster Pack – Blooming Radiance".into());
        // a stale translation goes away once official
        products.get_mut("hBP01").unwrap().unofficial_name = Localized::en("old".into());
        let titles = [
            "Booster hBP10 Glory Echo".to_string(),
            "Start Deck hSD20-21: Kyoko & Mela".to_string(),
        ];

        let sources = update_unofficial_names(&mut products, &all_cards, &pack_lines, &titles);
        // without the sheet's set number, like the short names of boosters
        assert_eq!(
            sources["hBP09"],
            UnofficialName::Pack("Volume Vortex".into())
        );
        assert_eq!(
            products["hBP09"].unofficial_name.english.as_deref(),
            Some("Volume Vortex")
        );
        // the best line is another product's: on 1 of the 2 cards with it
        assert!(matches!(sources["hCO01"], UnofficialName::None(Some(_))));
        assert_eq!(products["hCO01"].unofficial_name, Localized::default());
        assert_eq!(sources["hBP01"], UnofficialName::Official);
        assert_eq!(products["hBP01"].unofficial_name, Localized::default());
        // no card yet: the tab about it
        assert_eq!(
            sources["hBP10"],
            UnofficialName::TabTitle("Glory Echo".into())
        );
    }

    #[test]
    fn test_drop_product_sources() {
        let printing = |codes: &[&str], sources: &[&str]| CardIllustration {
            card_number: "hBP01-001".into(),
            products: Localized::jp(codes.iter().map(|c| c.to_string()).collect()),
            promo_sources: Localized::jp(sources.iter().map(|c| c.to_string()).collect()),
            ..Default::default()
        };
        let mut all_cards = CardsDatabase::new();
        all_cards
            .entry("hBP01-001".into())
            .or_default()
            .illustrations = vec![
            // the name of its product
            printing(&["hWF01"], &["hololive Official Card Game Twin Wafers"]),
            // also a source of its own
            printing(
                &["hCS01"],
                &["1st Anniversary Celebration Set", "VJump June 2025"],
            ),
            // the name of another product
            printing(&["hPR"], &["1st Anniversary Celebration Set"]),
        ];
        let mut products = ProductsDatabase::new();
        products.insert(
            "hWF01".into(),
            Product {
                unofficial_name: Localized::en("hololive Official Card Game Twin Wafers".into()),
                ..Default::default()
            },
        );
        products.insert(
            "hCS01".into(),
            Product {
                name: Localized::jp(
                    "【hololive production OFFICIAL SHOP限定】1st Anniversary Celebration Set"
                        .into(),
                ),
                ..Default::default()
            },
        );
        products.insert("hPR".into(), Product::default());

        assert_eq!(drop_product_sources(&mut all_cards, &products), 2);
        let sources = all_cards["hBP01-001"]
            .illustrations
            .iter()
            .map(|i| i.promo_sources.clone())
            .collect_vec();
        assert_eq!(sources[0], Localized::default());
        assert_eq!(sources[1], Localized::jp(vec!["VJump June 2025".into()]));
        assert_eq!(
            sources[2],
            Localized::jp(vec!["1st Anniversary Celebration Set".into()])
        );
    }

    #[test]
    fn test_unofficial_names_from_sources() {
        let mut all_cards = CardsDatabase::new();
        let mut printing = |number: &str, codes: &[&str], sources: &[&str]| {
            all_cards
                .entry(number.into())
                .or_default()
                .illustrations
                .push(CardIllustration {
                    card_number: number.into(),
                    products: Localized::jp(codes.iter().map(|c| c.to_string()).collect()),
                    promo_sources: Localized::jp(sources.iter().map(|c| c.to_string()).collect()),
                    ..Default::default()
                });
        };
        let wafers = "hololive Official Card Game Twin Wafers";
        for n in 1..=4 {
            printing(&format!("hBP01-00{n}"), &["hWF01"], &[wafers]);
        }
        printing("hBP01-005", &["hWF01"], &[]);
        // a promo source that is not a product: on the cards of many products
        printing("hBP02-001", &["hPR"], &["VJump November 2024"]);
        printing("hBP02-002", &["hCO02"], &["VJump November 2024"]);
        printing("hBP02-003", &["hCO02"], &[]);
        let mut products: ProductsDatabase = ["hWF01", "hCO02", "hPR"]
            .into_iter()
            .map(|c| (c.to_string(), Product::default()))
            .collect();

        let sources = update_unofficial_names(&mut products, &all_cards, &HashMap::new(), &[]);
        // as the sheet writes it
        assert_eq!(sources["hWF01"], UnofficialName::Source(wafers.into()));
        assert_eq!(
            products["hWF01"].unofficial_name.english.as_deref(),
            Some(wafers)
        );
        assert!(matches!(sources["hCO02"], UnofficialName::None(_)));
        assert_eq!(products["hCO02"].unofficial_name, Localized::default());
    }

    #[test]
    fn test_without_set_number() {
        assert_eq!(without_set_number("Set 9 Volume Vortex"), "Volume Vortex");
        assert_eq!(without_set_number("Set 10 Glory Echo"), "Glory Echo");
        assert_eq!(
            without_set_number("Start Deck Amane Kanata"),
            "Start Deck Amane Kanata"
        );
        assert_eq!(
            without_set_number("Start Deck Sets 2025 HoloNatsu Paradise ver"),
            "Start Deck Sets 2025 HoloNatsu Paradise ver"
        );
        assert_eq!(without_set_number("Set 9"), "Set 9");
    }

    #[test]
    fn test_tab_title_name() {
        let titles = [
            "Booster hBP10 Glory Echo".to_string(),
            "Start Deck hSD20-21: Kyoko & Mela".to_string(),
            "Start Deck hSD01 Tokino Sora & AZKi".to_string(),
        ];
        // boosters: the name alone, others with their kind
        assert_eq!(
            tab_title_name("hBP10", Some("boosters"), &titles).as_deref(),
            Some("Glory Echo")
        );
        assert_eq!(
            tab_title_name("hSD01", Some("decks"), &titles).as_deref(),
            Some("Start Deck Tokino Sora & AZKi")
        );
        // a tab about several products doesn't name one
        assert_eq!(tab_title_name("hSD20", Some("decks"), &titles), None);
        assert_eq!(tab_title_name("hCO01", None, &titles), None);
    }
}
