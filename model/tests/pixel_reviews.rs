use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use hocg_fan_sim_assets_model::{
    CardIllustration, CardRarity, Language,
    img_hash::{dist_hash, is_similar, to_image_hash},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewFile {
    version: u32,
    cards: Vec<CardReview>,
}

/// The confirmed grouping of every image of one card: images that share a `group`
/// number have the same illustration, every other pair is different.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CardReview {
    card_number: String,
    approval: Approval,
    images: Vec<ReviewedImage>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Approval {
    /// Checked individually.
    Manual,
    /// Pixel suggestion accepted in bulk.
    Bulk,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[serde(deny_unknown_fields)]
struct ReviewedImage {
    language: ImageLanguage,
    rarity: String,
    path: String,
    sha256: String,
    group: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum ImageLanguage {
    JP,
    EN,
}

impl ImageLanguage {
    fn to_language(&self) -> Language {
        match self {
            ImageLanguage::JP => Language::Japanese,
            ImageLanguage::EN => Language::English,
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            ImageLanguage::JP => "JP",
            ImageLanguage::EN => "EN",
        }
    }
}

#[derive(Debug, Default)]
struct Evaluation {
    pairs: usize,
    /// Different illustrations that is_similar groups together.
    false_merges: Vec<String>,
    /// Same illustration that is_similar keeps apart.
    false_splits: Vec<String>,
}

pub(super) fn resolve_asset(root: &Path, relative: &str) -> Result<PathBuf, String> {
    if relative.contains('\\') {
        return Err(format!("asset path must use forward slashes: {relative}"));
    }
    if relative
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(format!("invalid relative asset path: {relative}"));
    }
    let path = Path::new(relative);
    if !path.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err(format!("invalid relative asset path: {relative}"));
    }
    if !matches!(path.components().next(), Some(Component::Normal(dir)) if dir == "img" || dir == "img_en")
    {
        return Err(format!(
            "asset path must start with img or img_en: {relative}"
        ));
    }
    let root = root
        .canonicalize()
        .map_err(|e| format!("failed to resolve {}: {e}", root.display()))?;
    let path = root
        .join(path)
        .canonicalize()
        .map_err(|e| format!("failed to resolve {relative}: {e}"))?;
    if !path.starts_with(&root) {
        return Err(format!("asset path escapes assets directory: {relative}"));
    }
    Ok(path)
}

pub(super) fn sha256_file(path: &Path) -> Result<String, String> {
    let data = fs::read(path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    Ok(Sha256::digest(data)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn evaluate_reviews(
    file: &ReviewFile,
    assets: &Path,
    approvals: &[Approval],
) -> Result<Evaluation, String> {
    if file.version != 2 {
        return Err(format!(
            "unsupported review version: {} (expected 2)",
            file.version
        ));
    }
    if file.cards.is_empty() {
        return Err("review file contains no cards".to_owned());
    }
    let mut seen = HashSet::new();
    let mut fingerprints = HashMap::new();
    let mut result = Evaluation::default();
    for card in &file.cards {
        if card.card_number.trim().is_empty() {
            return Err("review has an empty card number".to_owned());
        }
        if !seen.insert(card.card_number.as_str()) {
            return Err(format!("duplicate review for {}", card.card_number));
        }
        if card.images.len() < 2 {
            return Err(format!(
                "{} review has fewer than 2 images",
                card.card_number
            ));
        }
        let mut paths = HashSet::new();
        let mut illustrations = Vec::new();
        for item in &card.images {
            if !paths.insert(item.path.as_str()) {
                return Err(format!("duplicate image in review: {}", item.path));
            }
            if item.rarity.trim().is_empty()
                || item.sha256.len() != 64
                || !item
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            {
                return Err(format!("invalid rarity or SHA-256 for {}", item.path));
            }
            let prefix = match item.language {
                ImageLanguage::JP => "img/",
                ImageLanguage::EN => "img_en/",
            };
            if !item.path.starts_with(prefix) {
                return Err(format!("language does not match image path: {}", item.path));
            }
            let path = resolve_asset(assets, &item.path)?;
            if !fingerprints.contains_key(&path) {
                fingerprints.insert(path.clone(), sha256_file(&path)?);
            }
            if fingerprints[&path] != item.sha256 {
                return Err(format!("image changed since review: {}", item.path));
            }
            if !approvals.contains(&card.approval) {
                continue;
            }
            let image = image::open(&path)
                .map_err(|e| format!("failed to open {}: {e}", path.display()))?;
            let language = item.language.to_language();
            let mut illustration = CardIllustration {
                card_number: card.card_number.clone(),
                rarity: CardRarity::from(item.rarity.clone()),
                img_hash: to_image_hash(&image.into_rgb8(), Some(language)),
                ..Default::default()
            };
            // unreleased images are stored in an "unreleased" folder and have no manage_id
            if !item.path.split('/').any(|part| part == "unreleased") {
                *illustration.manage_id.value_mut(language) = Some(BTreeSet::from([1]));
            }
            illustrations.push((item, illustration));
        }
        for (x, (item_a, a)) in illustrations.iter().enumerate() {
            for (item_b, b) in &illustrations[x + 1..] {
                result.pairs += 1;
                let expected = item_a.group == item_b.group;
                let actual = is_similar(a, b);
                if actual == expected {
                    continue;
                }
                let message = format!(
                    "{} {} {} ({}) vs {} {} ({}): distance {}",
                    card.card_number,
                    item_a.rarity,
                    item_a.language.as_str(),
                    item_a.path,
                    item_b.rarity,
                    item_b.language.as_str(),
                    item_b.path,
                    dist_hash(&a.img_hash, &b.img_hash)
                );
                if actual {
                    result.false_merges.push(message);
                } else {
                    result.false_splits.push(message);
                }
            }
        }
    }
    Ok(result)
}

/// Ground truth committed with the tests: the illustration groups of every card that was
/// reviewed manually.
const DEFAULT_REVIEWS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/card-image-reviews.json");

// Needs the assets folder. Set PIXEL_REVIEW_IN to check another review file, and
// PIXEL_REVIEW_APPROVAL=manual to check only individually reviewed cards.
#[ignore]
#[test]
fn test_reviewed_pairs_match_hash() {
    let input = std::env::var("PIXEL_REVIEW_IN").unwrap_or_else(|_| DEFAULT_REVIEWS.to_owned());
    let approvals: &[Approval] = match std::env::var("PIXEL_REVIEW_APPROVAL").as_deref() {
        Ok("manual") => &[Approval::Manual],
        Ok("bulk") => &[Approval::Bulk],
        Ok("all") | Err(_) => &[Approval::Manual, Approval::Bulk],
        Ok(other) => panic!("PIXEL_REVIEW_APPROVAL must be manual, bulk or all, not {other}"),
    };
    let content =
        fs::read_to_string(&input).unwrap_or_else(|e| panic!("failed to read {input}: {e}"));
    let file: ReviewFile = serde_json::from_str(&content)
        .unwrap_or_else(|e| panic!("invalid review file {input}: {e}"));
    let assets = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("assets");
    let result = evaluate_reviews(&file, &assets, approvals).unwrap_or_else(|e| panic!("{e}"));
    for mismatch in &result.false_merges {
        eprintln!("false merge: {mismatch}");
    }
    for mismatch in &result.false_splits {
        eprintln!("false split: {mismatch}");
    }
    let wrong = result.false_merges.len() + result.false_splits.len();
    println!(
        "{} cards, {} pairs: {} correct, {} false merges, {} false splits",
        file.cards
            .iter()
            .filter(|card| approvals.contains(&card.approval))
            .count(),
        result.pairs,
        result.pairs - wrong,
        result.false_merges.len(),
        result.false_splits.len()
    );
    assert_eq!(
        wrong, 0,
        "{wrong} of {} reviewed pairs disagree with is_similar",
        result.pairs
    );
}

#[test]
fn reviewed_pairs_detect_hash_mismatches_and_changed_images() {
    let temp = tempfile::tempdir().unwrap();
    let folder = temp.path().join("img");
    fs::create_dir(&folder).unwrap();
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("card_images");
    for name in ["hBP09-105_U.webp", "hBP09-105_P.webp"] {
        fs::copy(fixtures.join(name), folder.join(name)).unwrap();
    }
    let image = |name: &str, rarity: &str, group| ReviewedImage {
        language: ImageLanguage::JP,
        rarity: rarity.to_owned(),
        path: format!("img/{name}"),
        sha256: sha256_file(&folder.join(name)).unwrap(),
        group,
    };
    let all = [Approval::Manual, Approval::Bulk];
    let mut file = ReviewFile {
        version: 2,
        cards: vec![CardReview {
            card_number: "hBP09-105".to_owned(),
            approval: Approval::Manual,
            images: vec![
                image("hBP09-105_U.webp", "U", 1),
                image("hBP09-105_P.webp", "P", 2),
            ],
        }],
    };
    let result = evaluate_reviews(&file, temp.path(), &all).unwrap();
    assert_eq!(result.pairs, 1);
    assert!(result.false_merges.is_empty() && result.false_splits.is_empty());

    file.cards[0].images[1].group = 1;
    let result = evaluate_reviews(&file, temp.path(), &all).unwrap();
    assert_eq!(result.false_splits.len(), 1);
    let result = evaluate_reviews(&file, temp.path(), &[Approval::Bulk]).unwrap();
    assert_eq!(result.pairs, 0);

    file.cards[0].images[0].language = ImageLanguage::EN;
    assert!(
        evaluate_reviews(&file, temp.path(), &all)
            .unwrap_err()
            .contains("language does not match")
    );
    file.cards[0].images[0].language = ImageLanguage::JP;
    file.cards.push(file.cards[0].clone());
    assert!(
        evaluate_reviews(&file, temp.path(), &all)
            .unwrap_err()
            .contains("duplicate review")
    );
    file.cards.pop();
    fs::write(folder.join("hBP09-105_P.webp"), b"changed").unwrap();
    assert!(
        evaluate_reviews(&file, temp.path(), &all)
            .unwrap_err()
            .contains("image changed")
    );
}

#[test]
fn reviewed_asset_paths_reject_escapes() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for path in [
        "../secret",
        "img/../../secret",
        "img\\file",
        "other/file",
        "img//file",
        "img/./file",
    ] {
        assert!(resolve_asset(root, path).is_err(), "{path}");
    }
}
