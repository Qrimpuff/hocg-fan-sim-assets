//! Image hashes used to recognize card illustrations.
//!
//! A hash has these components (`v3|H32=..|CYM=..|L=..`):
//! - `H32`: 32x32 double-gradient perceptual hash of the whole card. Robust to scans,
//!   compression and small crops; used to match images from other sources.
//! - `CYM`: average color of the whole card.
//! - `L`: language of the image (`JP` or `EN`), when known.
//!
//! [`dist_hash`] compares the perceptual hash and the average color. It tolerates scans,
//! photos, crops and "SAMPLE" watermarks, so it is used to match images from other
//! sources. It lives in the model crate, which needs it to resolve card references made
//! from an image hash; everything else about hashes is internal to the CLI.
//!
//! [`is_similar`] decides whether two illustrations show the same artwork. Between
//! official images (both released), it compares their [`Grid`]s with thresholds that only
//! depend on whether the language and the rarity of both images are the same. The
//! thresholds were fitted on manually reviewed illustration groups (see the pixel review
//! tests). Unreleased images can be samples, scans or crops, so they are compared with
//! the tolerant [`dist_hash`] instead.
//!
//! Grids are not stored in the hash: [`ImageGrids`] makes them from the saved images when
//! they are needed, and keeps them for the rest of the run. It also makes the placeholders
//! of the images ([`to_placeholder`]), stored next to the hash as a preview of the image.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

pub use hocg_fan_sim_assets_model::dist_hash;
use hocg_fan_sim_assets_model::{CardIllustration, Language, cardhash};
use image::{GrayImage, RgbImage, imageops};
use image_hasher::HasherConfig;
use itertools::Itertools;
use palette::{Hsv, IntoColor, Srgb};
use parking_lot::Mutex;

const DEBUG: bool = false;

// one image can map to multiple illustrations, if they are similar enough
pub const DIST_TOLERANCE_DIFF_RARITY: u64 = 100; // equivalent to 10% differences
pub const DIST_TOLERANCE_SAME_RARITY: u64 = DIST_TOLERANCE_DIFF_RARITY + 38;

const VERSION: &str = "v3";

const KEY_HASH: &str = "H32";
const KEY_COLOR: &str = "CYM";
const KEY_LANGUAGE: &str = "L";

const HASH_WIDTH: u32 = 32;

const GRID_WIDTH: usize = 10;
const GRID_HEIGHT: usize = 14;
const GRID_CELLS: usize = GRID_WIDTH * GRID_HEIGHT;
/// Rows of the grid that hold the artwork on every card layout (from 7% to 58% of the
/// height). Name bars, text boxes, rarity marks and footers are outside of it, so text
/// differences there do not count as local differences.
const GRID_ART_ROWS: std::ops::Range<usize> = 1..8;
/// Rows of the grid without the name bar (row 0) and the text box, tags and footer (rows
/// 11 to 13). On full-art cards (e.g. HR), this text is printed over the artwork, so a
/// translation changes these rows across the whole width.
const GRID_TEXT_FREE_ROWS: std::ops::Range<usize> = 1..11;
/// All the rows of the grid.
const GRID_ALL_ROWS: std::ops::Range<usize> = 0..GRID_HEIGHT;

/// Thresholds of [`is_similar`] on the grid comparison, in 0-255 color units.
/// The average difference covers the `mean_rows` of the card; the local difference is the
/// largest cell difference in the art rows.
struct GridThresholds {
    mean_rows: std::ops::Range<usize>,
    mean: f32,
    local: f32,
}

fn grid_thresholds(same_language: bool, same_rarity: bool) -> GridThresholds {
    match (same_language, same_rarity) {
        // two prints of the same rarity and language are only the same if nearly identical
        (true, true) => GridThresholds {
            mean_rows: GRID_ALL_ROWS,
            mean: 4.5,
            local: 9.0,
        },
        // e.g. common and promo versions: different rarity marks and foil
        (true, false) => GridThresholds {
            mean_rows: GRID_ALL_ROWS,
            mean: 4.5,
            local: 22.0,
        },
        // translations: text, and localized stamps that can overlap the art. The translated
        // name and text are left out of the average: on full-art cards they cover the art
        // and push it over the limit (e.g. the hBP07 HRs, from 6.7 to 7.8 on the whole card)
        (false, true) => GridThresholds {
            mean_rows: GRID_TEXT_FREE_ROWS,
            mean: 7.0,
            local: 70.0,
        },
        (false, false) => GridThresholds {
            mean_rows: GRID_ALL_ROWS,
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

/// A card reduced to a coarse `GRID_WIDTH` x `GRID_HEIGHT` grid of average RGB colors,
/// row by row. Comparing grids reveals local changes (stamps, emblems, frames, foil) that
/// the perceptual hash ignores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grid(Arc<[u8]>);

impl Grid {
    /// Average RGB color of each cell of the grid of an image.
    pub fn of(img: &RgbImage) -> Self {
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
        Self(grid.into())
    }
}

/// The grid of an illustration, and the language of the image it was made from.
#[derive(Debug, Clone)]
pub struct IllustrationGrid {
    pub language: Language,
    pub grid: Grid,
}

/// What is kept of a decoded image.
#[derive(Debug, Clone)]
struct ImageSummary {
    grid: Grid,
    placeholder: Arc<str>,
}

impl ImageSummary {
    fn of(img: &RgbImage) -> Self {
        Self {
            grid: Grid::of(img),
            placeholder: to_placeholder(img).into(),
        }
    }
}

/// Makes the grids and placeholders of the saved images of illustrations when they are
/// needed, and keeps them for the rest of the run. They are made again when the image file
/// changes.
pub struct ImageGrids {
    images_jp_path: PathBuf,
    images_en_path: PathBuf,
    /// by image path: the modification time of the file, and its summary (`None` if it
    /// cannot be decoded)
    images: Mutex<HashMap<PathBuf, (SystemTime, Option<ImageSummary>)>>,
}

impl ImageGrids {
    pub fn new(images_jp_path: &Path, images_en_path: &Path) -> Self {
        Self {
            images_jp_path: images_jp_path.to_owned(),
            images_en_path: images_en_path.to_owned(),
            images: Default::default(),
        }
    }

    /// The image the grid of an illustration is made from: the image its hash comes from
    /// for an official illustration (see [`hash_image_language`]), otherwise its saved
    /// image, the Japanese one first.
    pub fn image_of(&self, illust: &CardIllustration) -> Option<(Language, PathBuf)> {
        let language = hash_image_language(illust).or_else(|| {
            [Language::Japanese, Language::English]
                .into_iter()
                .find(|&language| illust.img_path.value(language).is_some())
        })?;
        let folder = match language {
            Language::Japanese => &self.images_jp_path,
            Language::English => &self.images_en_path,
        };
        let path = folder.join(illust.img_path.value(language).as_deref()?);
        Some((language, path))
    }

    /// The grid of an illustration, `None` if its image is missing or cannot be decoded.
    pub fn grid(&self, illust: &CardIllustration) -> Option<IllustrationGrid> {
        let (language, path) = self.image_of(illust)?;
        let grid = self.summary_at(&path)?.grid;
        Some(IllustrationGrid { language, grid })
    }

    /// The placeholder of the image of an illustration (the image its grid is made from, see
    /// [`Self::image_of`]), `None` if its image is missing or cannot be decoded.
    pub fn placeholder(&self, illust: &CardIllustration) -> Option<String> {
        let (_, path) = self.image_of(illust)?;
        Some(self.summary_at(&path)?.placeholder.to_string())
    }

    fn summary_at(&self, path: &Path) -> Option<ImageSummary> {
        let modified = fs::metadata(path).and_then(|m| m.modified()).ok()?;
        if let Some((time, summary)) = self.images.lock().get(path)
            && *time == modified
        {
            return summary.clone();
        }
        // decoded without the lock, so that images can be decoded in parallel
        let summary = image::open(path)
            .ok()
            .map(|img| ImageSummary::of(&img.into_rgb8()));
        self.images
            .lock()
            .insert(path.to_owned(), (modified, summary.clone()));
        summary
    }

    /// Keeps the grid and placeholder of an image file that was just decoded for another
    /// reason (e.g. to hash it), so that it is not decoded again.
    pub fn remember(&self, path: &Path, img: &RgbImage) {
        if let Ok(modified) = fs::metadata(path).and_then(|m| m.modified()) {
            self.images
                .lock()
                .insert(path.to_owned(), (modified, Some(ImageSummary::of(img))));
        }
    }
}

/// The placeholder of a card image: a short string ([`cardhash`]) that decodes to a blurred
/// preview of the card.
pub fn to_placeholder(img: &RgbImage) -> String {
    cardhash::encode(img.as_raw(), img.width(), img.height())
}

/// Removes the grid (`G`) from a hash: older hashes have one, grids are now made from the
/// images when they are needed.
pub fn strip_grid(hash: &mut String) {
    if hash.split('|').any(|part| part.starts_with("G=")) {
        *hash = hash
            .split('|')
            .filter(|part| !part.starts_with("G="))
            .join("|");
    }
}

/// Per-cell average absolute color difference between two grids.
fn grid_cell_differences(a: &Grid, b: &Grid) -> Vec<f32> {
    a.0.as_chunks::<3>()
        .0
        .iter()
        .zip(b.0.as_chunks::<3>().0)
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
    /// average cell difference over the given rows
    mean: f32,
    /// largest cell difference in the art rows
    local: f32,
}

fn grid_distance(a: &Grid, b: &Grid, mean_rows: std::ops::Range<usize>) -> GridDistance {
    let cells = grid_cell_differences(a, b);
    let mean_cells = &cells[mean_rows.start * GRID_WIDTH..mean_rows.end * GRID_WIDTH];
    let mean = mean_cells.iter().sum::<f32>() / mean_cells.len() as f32;
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
pub fn is_similar(into: &CardIllustration, from: &CardIllustration, grids: &ImageGrids) -> bool {
    let same_rarity = into.rarity == from.rarity;
    // official images are clean: compare them in detail
    if into.manage_id.has_value()
        && from.manage_id.has_value()
        && let Some(a) = grids.grid(into)
        && let Some(b) = grids.grid(from)
    {
        let same_language = a.language == b.language;
        if same_rarity
            && !same_language
            && into.card_number == from.card_number
            && SAME_ACROSS_LANGUAGES
                .contains(&(into.card_number.as_str(), &into.rarity.to_string()))
        {
            return true;
        }
        let thresholds = grid_thresholds(same_language, same_rarity);
        let distance = grid_distance(&a.grid, &b.grid, thresholds.mean_rows.clone());
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

    // unreleased images (samples, scans, crops), and official images that are missing
    let dist = dist_hash(&into.img_hash, &from.img_hash);
    dist <= if same_rarity {
        DIST_TOLERANCE_SAME_RARITY
    } else {
        DIST_TOLERANCE_DIFF_RARITY
    }
}

/// The average color difference (0-255) between the artworks of two images: the cells of the
/// art rows of their grids ([`GRID_ART_ROWS`]), without the frame, the name bar and the text
/// box. Cards of a same kind (e.g. two staff supports) share most of their frame and text box,
/// that dominate [`dist_hash`].
pub fn art_distance(a: &Grid, b: &Grid) -> f32 {
    let cells = grid_cell_differences(a, b);
    let art = &cells[GRID_ART_ROWS.start * GRID_WIDTH..GRID_ART_ROWS.end * GRID_WIDTH];
    art.iter().sum::<f32>() / art.len() as f32
}

pub fn can_merge(into: &CardIllustration, from: &CardIllustration, grids: &ImageGrids) -> bool {
    // if both have manage_id, they must be identical
    for language in [Language::Japanese, Language::English] {
        if into.manage_id.value(language).is_some()
            && from.manage_id.value(language).is_some()
            && into.img_hash != from.img_hash
        {
            return false;
        }
    }

    is_similar(into, from, grids)
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

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, fs::File, time::Duration};

    use hocg_fan_sim_assets_model::Localized;

    use super::*;

    fn solid(color: [u8; 3]) -> RgbImage {
        RgbImage::from_pixel(40, 56, image::Rgb(color))
    }

    fn save(path: &Path, img: &RgbImage, modified: SystemTime) {
        img.save(path).unwrap();
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
    }

    #[test]
    fn grids_are_made_from_the_images_and_follow_their_changes() {
        let dir = tempfile::tempdir().unwrap();
        let grids = ImageGrids::new(dir.path(), dir.path());
        let path = dir.path().join("card.png");
        let illust = CardIllustration {
            manage_id: Localized::jp(BTreeSet::from([1])),
            img_path: Localized::jp("card.png".to_owned()),
            ..Default::default()
        };

        // no image yet
        assert!(grids.grid(&illust).is_none());

        let time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        save(&path, &solid([200, 10, 10]), time);
        let red = grids.grid(&illust).unwrap();
        assert_eq!(red.language, Language::Japanese);
        assert_eq!(red.grid, Grid::of(&solid([200, 10, 10])));

        assert_eq!(
            grids.placeholder(&illust).unwrap(),
            to_placeholder(&solid([200, 10, 10]))
        );

        // the image is replaced: its grid and placeholder are made again
        save(&path, &solid([10, 10, 200]), time + Duration::from_secs(1));
        assert_eq!(
            grids.grid(&illust).unwrap().grid,
            Grid::of(&solid([10, 10, 200]))
        );
        assert_eq!(
            grids.placeholder(&illust).unwrap(),
            to_placeholder(&solid([10, 10, 200]))
        );
    }

    #[test]
    fn grids_of_unreleased_illustrations_come_from_their_saved_image() {
        let grids = ImageGrids::new(Path::new("jp"), Path::new("en"));
        let unreleased = CardIllustration {
            img_path: Localized::en("card.webp".to_owned()),
            ..Default::default()
        };
        assert_eq!(
            grids.image_of(&unreleased),
            Some((Language::English, Path::new("en").join("card.webp")))
        );
        // an official illustration: the image its hash comes from
        let official = CardIllustration {
            manage_id: Localized::en(BTreeSet::from([1])),
            img_path: Localized {
                japanese: Some("jp.webp".to_owned()),
                english: Some("en.webp".to_owned()),
            },
            ..Default::default()
        };
        assert_eq!(
            grids.image_of(&official),
            Some((Language::English, Path::new("en").join("en.webp")))
        );
    }

    #[test]
    fn grids_are_removed_from_older_hashes() {
        let mut hash = "v3|H32=abc|CYM=0C0F1C|G=AAAA|L=JP".to_owned();
        strip_grid(&mut hash);
        assert_eq!(hash, "v3|H32=abc|CYM=0C0F1C|L=JP");
        strip_grid(&mut hash);
        assert_eq!(hash, "v3|H32=abc|CYM=0C0F1C|L=JP");
        assert!(!to_image_hash(&solid([1, 2, 3]), Some(Language::English)).contains("|G="));
    }
}
