//! Reading of the image hashes stored in the card database.
//!
//! Hashes are computed by the assets CLI (`hocg-fan-sim-assets-cli`), which also decides
//! which illustrations show the same artwork. This crate only needs to compare a hash with
//! the stored ones, to find an illustration from a [`crate::CardReference`] that was made
//! from an image hash.
//!
//! A hash looks like `v3|H32=..|CYM=..|..`. Only these components are read here:
//! - `H32`: 32x32 double-gradient perceptual hash of the whole card.
//! - `CYM`: average color of the whole card.
//!
//! Other components are ignored, so hashes from v2 onward can be compared.

use std::collections::HashMap;

use image_hasher::ImageHash;

const KEY_HASH: &str = "H32";
const KEY_COLOR: &str = "CYM";

const HASH_BITS: u32 = 32 * 32;
const WEIGHT_HASH: f32 = 0.925;
const WEIGHT_COLOR: f32 = 0.30;

struct ParsedHash<'a> {
    components: HashMap<&'a str, &'a str>,
}

impl<'a> ParsedHash<'a> {
    fn parse(s: &'a str) -> Option<Self> {
        // the first part is the version
        let mut components = HashMap::new();
        for part in s.split('|').skip(1) {
            let (k, v) = part.split_once('=')?;
            components.insert(k, v);
        }
        Some(Self { components })
    }

    fn get(&self, key: &str) -> Option<&'a str> {
        self.components.get(key).copied()
    }

    fn hash(&self) -> Option<ImageHash<Box<[u8]>>> {
        ImageHash::from_base64(self.get(KEY_HASH)?).ok()
    }

    fn color(&self) -> Option<[u8; 3]> {
        let c = self.get(KEY_COLOR)?;
        if c.len() != 6 {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(c.get(i..i + 2)?, 16).ok();
        Some([byte(0)?, byte(2)?, byte(4)?])
    }
}

/// Distance between two image hashes, mostly from 0 (identical) to 1000, from the
/// perceptual hash and the average color. These are computed and weighted the same way
/// as in v2, so v2 hashes can still be compared. Hashes whose perceptual hash cannot be
/// read are at the maximum distance.
pub fn dist_hash(h1: &str, h2: &str) -> u64 {
    (dist_hash_norm(h1, h2).unwrap_or(1.0) * 1000.0) as u64 // 1.0 is the minimum distance, so we scale it up
}

fn dist_hash_norm(h1: &str, h2: &str) -> Option<f32> {
    let a = ParsedHash::parse(h1)?;
    let b = ParsedHash::parse(h2)?;

    // without the perceptual hash, the color alone would look like a near match
    let mut score = a.hash()?.dist(&b.hash()?) as f32 / HASH_BITS as f32 * WEIGHT_HASH;
    if let (Some(x), Some(y)) = (a.color(), b.color()) {
        let d = x
            .iter()
            .zip(y)
            .map(|(x, y)| (*x as f32 - y as f32).powi(2))
            .sum::<f32>()
            .sqrt()
            / (3.0f32 * 255.0 * 255.0).sqrt();
        score += d * WEIGHT_COLOR;
    }
    Some(score)
}
