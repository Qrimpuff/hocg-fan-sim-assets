//! Image hashes used to recognize card illustrations.
//!
//! A v3 hash has these components (`v3|H32=..|CYM=..|G=..|L=..`):
//! - `H32`: 32x32 double-gradient perceptual hash of the whole card. Robust to scans,
//!   compression and small crops; used to match images from other sources.
//! - `CYM`: average color of the whole card.
//! - `G`: the card reduced to a coarse 10x14 grid of average colors. Comparing grids
//!   reveals local changes (stamps, emblems, frames, foil) that the perceptual hash
//!   ignores.
//! - `L`: language of the image (`JP` or `EN`), when known.
//!
//! [`dist_hash`] compares the perceptual hash and the average color. It tolerates scans,
//! photos, crops and "SAMPLE" watermarks, so it is used to match images from other
//! sources.
//!
//! [`is_similar`] decides whether two illustrations show the same artwork. Between
//! official images (both released), it compares the grids with thresholds that only
//! depend on whether the language and the rarity of both images are the same. The
//! thresholds were fitted on manually reviewed illustration groups (see the pixel review
//! tests). Unreleased images can be samples, scans or crops, so they are compared with
//! the tolerant [`dist_hash`] instead.

use std::path::Path;

use crate::{CardIllustration, Language};
use base64::{Engine, engine::general_purpose::STANDARD_NO_PAD};
use image::{GrayImage, RgbImage, imageops};
use image_hasher::{HasherConfig, ImageHash};
use itertools::Itertools;

use std::collections::HashMap;

use palette::{Hsv, IntoColor, Srgb};

pub const DEBUG: bool = false;

// one image can map to multiple illustrations, if they are similar enough
pub const DIST_TOLERANCE_DIFF_RARITY: u64 = 100; // equivalent to 10% differences
pub const DIST_TOLERANCE_SAME_RARITY: u64 = DIST_TOLERANCE_DIFF_RARITY + 38;

const VERSION_COMPONENT: &str = "version";
const VERSION: &str = "v3";

const KEY_HASH: &str = "H32";
const KEY_COLOR: &str = "CYM";
const KEY_GRID: &str = "G";
const KEY_LANGUAGE: &str = "L";

const HASH_WIDTH: u32 = 32;
const HASH_BITS: u32 = HASH_WIDTH * HASH_WIDTH;
const WEIGHT_HASH: f32 = 0.925;
const WEIGHT_COLOR: f32 = 0.30;

const GRID_WIDTH: usize = 10;
const GRID_HEIGHT: usize = 14;
const GRID_CELLS: usize = GRID_WIDTH * GRID_HEIGHT;
/// Rows of the grid that hold the artwork on every card layout (from 7% to 58% of the
/// height). Name bars, text boxes, rarity marks and footers are outside of it, so text
/// differences there do not count as local differences.
const GRID_ART_ROWS: std::ops::Range<usize> = 1..8;

/// Thresholds of [`is_similar`] on the grid comparison, in 0-255 color units.
/// The average difference covers the whole card; the local difference is the largest
/// cell difference in the art rows.
struct GridThresholds {
    mean: f32,
    local: f32,
}

fn grid_thresholds(same_language: bool, same_rarity: bool) -> GridThresholds {
    match (same_language, same_rarity) {
        // two prints of the same rarity and language are only the same if nearly identical
        (true, true) => GridThresholds {
            mean: 4.5,
            local: 9.0,
        },
        // e.g. common and promo versions: different rarity marks and foil
        (true, false) => GridThresholds {
            mean: 4.5,
            local: 22.0,
        },
        // translations: text, and localized stamps that can overlap the art
        (false, true) => GridThresholds {
            mean: 6.5,
            local: 70.0,
        },
        (false, false) => GridThresholds {
            mean: 6.5,
            local: 22.0,
        },
    }
}

/// Cards and rarities whose Japanese and English prints show the same artwork, but with a
/// layout difference that the grid cannot absorb. Any Japanese print of that card and
/// rarity matches any English one, so this assumes a single illustration per entry.
const SAME_ACROSS_LANGUAGES: &[(&str, &str)] = &[
    // the English promo rotates and shifts the art frame
    ("hBP02-080", "P"),
];

struct ParsedHash<'a> {
    components: HashMap<&'a str, &'a str>,
}

impl<'a> ParsedHash<'a> {
    fn parse(s: &'a str) -> Option<Self> {
        let mut components = HashMap::new();
        for (i, part) in s.split('|').enumerate() {
            if i == 0 {
                // starts with version
                components.insert(VERSION_COMPONENT, part);
            } else {
                let (k, v) = part.split_once('=')?;
                components.insert(k, v);
            }
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

    fn grid(&self) -> Option<Vec<u8>> {
        let grid = STANDARD_NO_PAD.decode(self.get(KEY_GRID)?).ok()?;
        (grid.len() == GRID_CELLS * 3).then_some(grid)
    }

    fn language(&self) -> Option<&'a str> {
        self.get(KEY_LANGUAGE)
    }
}

/// Per-cell average absolute color difference between two grids.
fn grid_cell_differences(a: &[u8], b: &[u8]) -> Vec<f32> {
    a.as_chunks::<3>()
        .0
        .iter()
        .zip(b.as_chunks::<3>().0)
        .map(|(a, b)| {
            a.iter()
                .zip(b)
                .map(|(a, b)| a.abs_diff(*b) as f32)
                .sum::<f32>()
                / 3.0
        })
        .collect()
}

struct GridDistance {
    /// average cell difference over the whole card
    mean: f32,
    /// largest cell difference in the art rows
    local: f32,
}

fn grid_distance(a: &[u8], b: &[u8]) -> GridDistance {
    let cells = grid_cell_differences(a, b);
    let mean = cells.iter().sum::<f32>() / cells.len() as f32;
    let local = cells[GRID_ART_ROWS.start * GRID_WIDTH..GRID_ART_ROWS.end * GRID_WIDTH]
        .iter()
        .copied()
        .fold(0.0, f32::max);
    GridDistance { mean, local }
}

fn prep_for_hash(img: &RgbImage) -> GrayImage {
    // use saturation for better gray
    let mut sat = sat_gray_image(img);

    // blur the "sample" part of the image
    let start_x = 0;
    let end_x = img.width();
    let start_y = img.height() * 170 / 560;
    let end_y = img.height() * 340 / 560;
    let cropped =
        imageops::crop_imm(&sat, start_x, start_y, end_x - start_x, end_y - start_y).to_image();
    let blurred = imageops::fast_blur(&cropped, 20.0);
    imageops::overlay(&mut sat, &blurred, start_x as i64, start_y as i64);

    sat
}

fn perceptual_hash(img: &RgbImage) -> String {
    let hasher = HasherConfig::new()
        .hash_size(HASH_WIDTH, HASH_WIDTH)
        .preproc_dct()
        .hash_alg(image_hasher::HashAlg::DoubleGradient)
        .to_hasher();
    hasher.hash_image(&prep_for_hash(img)).to_base64()
}

fn average_color(img: &RgbImage) -> String {
    // Average color (bias) component CYM=CCYYMM (hex)
    let (mut sum_c, mut sum_y, mut sum_m) = (0u64, 0u64, 0u64);
    for p in img.pixels() {
        let cymk = cymk(p.0);
        sum_c += (cymk.0 * 255.0) as u64;
        sum_y += (cymk.1 * 255.0) as u64;
        sum_m += (cymk.2 * 255.0) as u64;
    }
    let n = (img.width() as u64 * img.height() as u64).max(1);
    format!("{:02X}{:02X}{:02X}", sum_c / n, sum_y / n, sum_m / n)
}

/// Average RGB color of each cell of a `GRID_WIDTH` x `GRID_HEIGHT` grid.
fn color_grid(img: &RgbImage) -> Vec<u8> {
    let (width, height) = (img.width() as usize, img.height() as usize);
    let mut grid = Vec::with_capacity(GRID_CELLS * 3);
    for row in 0..GRID_HEIGHT {
        let (y0, y1) = (row * height / GRID_HEIGHT, (row + 1) * height / GRID_HEIGHT);
        for column in 0..GRID_WIDTH {
            let (x0, x1) = (
                column * width / GRID_WIDTH,
                (column + 1) * width / GRID_WIDTH,
            );
            let mut sum = [0u64; 3];
            for y in y0..y1 {
                for x in x0..x1 {
                    let pixel = img.get_pixel(x as u32, y as u32).0;
                    for (total, value) in sum.iter_mut().zip(pixel) {
                        *total += u64::from(value);
                    }
                }
            }
            let count = ((x1 - x0) * (y1 - y0)).max(1) as u64;
            grid.extend(sum.map(|total| ((total + count / 2) / count) as u8));
        }
    }
    grid
}

fn language_code(language: Language) -> &'static str {
    match language {
        Language::Japanese => "JP",
        Language::English => "EN",
    }
}

/// The language of the image an illustration's hash is computed from: the official
/// Japanese image when there is one, otherwise the official English image. Unreleased
/// illustrations have none: their hash comes from wherever their image was found (e.g. a
/// sheet thumbnail), not from the saved image.
pub fn hash_image_language(illust: &CardIllustration) -> Option<Language> {
    [Language::Japanese, Language::English]
        .into_iter()
        .find(|&language| {
            illust.manage_id.value(language).is_some() && illust.img_path.value(language).is_some()
        })
}

pub fn path_to_image_hash(path: &Path, language: Option<Language>) -> String {
    let img = image::open(path).unwrap().into_rgb8();
    to_image_hash(&img, language)
}

/// Computes the hash of a card image. Pass the language of the card when it is known,
/// so that [`is_similar`] can tell translations apart from different illustrations.
pub fn to_image_hash(img: &RgbImage, language: Option<Language>) -> String {
    let mut components = vec![
        (KEY_HASH, perceptual_hash(img)),
        (KEY_COLOR, average_color(img)),
        (KEY_GRID, STANDARD_NO_PAD.encode(color_grid(img))),
    ];
    if let Some(language) = language {
        components.push((KEY_LANGUAGE, language_code(language).to_owned()));
    }
    let hash =
        VERSION.to_owned() + "|" + &components.iter().map(|(k, v)| format!("{k}={v}")).join("|");
    if DEBUG {
        println!("{hash}");
    }
    hash
}

/// Whether two illustrations of a card show the same artwork.
pub fn is_similar(into: &CardIllustration, from: &CardIllustration) -> bool {
    let same_rarity = into.rarity == from.rarity;
    // official images are clean: compare them in detail
    if into.manage_id.has_value()
        && from.manage_id.has_value()
        && let (Some(a), Some(b)) = (
            ParsedHash::parse(&into.img_hash),
            ParsedHash::parse(&from.img_hash),
        )
        && let (Some(grid_a), Some(grid_b)) = (a.grid(), b.grid())
    {
        // unknown languages are treated as different, the more tolerant case
        let same_language = a.language().is_some() && a.language() == b.language();
        if same_rarity
            && a.language().is_some()
            && b.language().is_some()
            && !same_language
            && into.card_number == from.card_number
            && SAME_ACROSS_LANGUAGES
                .contains(&(into.card_number.as_str(), &into.rarity.to_string()))
        {
            return true;
        }
        let thresholds = grid_thresholds(same_language, same_rarity);
        let distance = grid_distance(&grid_a, &grid_b);
        if DEBUG {
            println!(
                "is_similar({} {}, {} {}): grid mean={:.2} local={:.1}",
                into.card_number,
                into.rarity,
                from.card_number,
                from.rarity,
                distance.mean,
                distance.local
            );
        }
        return distance.mean <= thresholds.mean && distance.local <= thresholds.local;
    }

    // unreleased images (samples, scans, crops) and hashes from before v3
    let dist = dist_hash(&into.img_hash, &from.img_hash);
    dist <= if same_rarity {
        DIST_TOLERANCE_SAME_RARITY
    } else {
        DIST_TOLERANCE_DIFF_RARITY
    }
}

pub fn can_merge(into: &CardIllustration, from: &CardIllustration) -> bool {
    // if both have manage_id, they must be identical
    for language in [Language::Japanese, Language::English] {
        if into.manage_id.value(language).is_some()
            && from.manage_id.value(language).is_some()
            && into.img_hash != from.img_hash
        {
            return false;
        }
    }

    is_similar(into, from)
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

    if DEBUG {
        println!("{VERSION} distance score={score:.4}");
    }
    Some(score)
}

fn sat_gray_image(img: &RgbImage) -> GrayImage {
    GrayImage::from_raw(
        img.width(),
        img.height(),
        img.pixels()
            .flat_map(|p| {
                let s = hsv(p.0).1;
                [((1.0 - s) * 255.0) as u8]
            })
            .collect(),
    )
    .unwrap()
}

fn hsv(ps: [u8; 3]) -> (f32, f32, f32) {
    let rgb = Srgb::from(ps).into_format();
    let hsv: Hsv = rgb.into_color();
    let h = hsv.hue.into_positive_degrees();
    let s = hsv.saturation;
    let v = hsv.value;
    (h, s, v)
}

// convert RGB to CYMK
fn cymk(rgb: [u8; 3]) -> (f32, f32, f32, f32) {
    let r = rgb[0] as f32 / 255.0;
    let g = rgb[1] as f32 / 255.0;
    let b = rgb[2] as f32 / 255.0;

    let k = 1.0 - r.max(g).max(b);
    if k >= 1.0 {
        return (0.0, 0.0, 0.0, 1.0);
    }

    let c = (1.0 - r - k) / (1.0 - k);
    let m = (1.0 - g - k) / (1.0 - k);
    let y = (1.0 - b - k) / (1.0 - k);

    (c, m, y, k)
}
