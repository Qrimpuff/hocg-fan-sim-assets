use std::collections::BTreeMap;

use hocg_fan_sim_assets_model::{
    CardIllustration,
    img_hash::{DIST_TOLERANCE_DIFF_RARITY, is_similar},
};

pub trait TrimOnce {
    fn trim_start_once(&self, pat: &str) -> &str;
    fn trim_end_once(&self, pat: &str) -> &str;
}

impl TrimOnce for str {
    fn trim_start_once(&self, pat: &str) -> &str {
        self.strip_prefix(pat).unwrap_or(self)
    }

    fn trim_end_once(&self, pat: &str) -> &str {
        self.strip_suffix(pat).unwrap_or(self)
    }
}

pub fn sanitize_filename(filename: &str) -> String {
    filename
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\' | '/' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect()
}

#[must_use]
pub fn clean_text(text: &str) -> String {
    // anything that needs to be done to have clean text goes here
    // I tried unicode-normalization but it messed up some characters
    text.trim().to_string()
}

/// Assigns external images (holoDelta arts, holoBuri art keys, store listings) to
/// illustrations, going through the matches from the best distance. An image always goes
/// to its best illustration. It is also shared with other illustrations within tolerance,
/// e.g. the same art in another rarity, but only if they show the same artwork as that
/// best illustration: variants such as prize cards look alike, but are different.
pub struct SharedImageMatches<K> {
    best: BTreeMap<K, (u64, CardIllustration)>,
}

impl<K: Ord> Default for SharedImageMatches<K> {
    fn default() -> Self {
        Self {
            best: BTreeMap::new(),
        }
    }
}

impl<K: Ord> SharedImageMatches<K> {
    /// Whether the image `key` should be assigned to `illust`. Call in order of distance.
    pub fn accept(&mut self, key: K, illust: &CardIllustration, dist: u64) -> bool {
        match self.best.get(&key) {
            None => {
                self.best.insert(key, (dist, illust.clone()));
                true
            }
            Some((best_dist, best)) => {
                dist <= (*best_dist).max(DIST_TOLERANCE_DIFF_RARITY) && is_similar(best, illust)
            }
        }
    }

    /// Whether the image `key` was assigned to any illustration.
    pub fn contains(&self, key: &K) -> bool {
        self.best.contains_key(key)
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, path::Path};

    use hocg_fan_sim_assets_model::{
        CardRarity, Language, Localized, img_hash::path_to_image_hash,
    };

    use super::*;

    fn illust(file: &str, rarity: CardRarity, manage_id: u32) -> CardIllustration {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../model/tests/card_images")
            .join(file);
        CardIllustration {
            card_number: file[..9].to_owned(),
            rarity,
            manage_id: Localized::jp(BTreeSet::from([manage_id])),
            img_hash: path_to_image_hash(&path, Some(Language::Japanese)),
            ..Default::default()
        }
    }

    #[test]
    fn shared_image_matches_only_share_the_same_artwork() {
        // promo with a different art box (different), and a common/promo with the same art
        let a = illust("hBP09-105_U.webp", CardRarity::Uncommon, 1);
        let b = illust("hBP09-105_P.webp", CardRarity::Promo, 2);
        let c = illust("hY01-001_C.webp", CardRarity::Common, 3);
        let s = illust("hY01-001_S.webp", CardRarity::from("S".to_owned()), 4);
        assert!(!is_similar(&a, &b));
        assert!(is_similar(&c, &s));

        let mut matches = SharedImageMatches::default();
        assert!(matches.accept("art", &a, 50));
        // close, but not the same artwork
        assert!(!matches.accept("art", &b, 60));
        assert!(matches.contains(&"art"));
        assert!(!matches.contains(&"other"));

        let mut matches = SharedImageMatches::default();
        assert!(matches.accept("art", &c, 50));
        assert!(matches.accept("art", &s, 60));
        // same artwork, but too far from the image
        assert!(!matches.accept("art", &s, DIST_TOLERANCE_DIFF_RARITY + 1));
    }
}
