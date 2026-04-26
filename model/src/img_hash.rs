use std::path::Path;

use crate::{CardIllustration, Language};
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
const V2_VERSION: &str = "v2";
const V2_WIDTH_DG32: u32 = 32;
const V2_BITS_DG32: u32 = V2_WIDTH_DG32 * V2_WIDTH_DG32;
const V2_WEIGHT_DG32: f32 = 0.925;
const V2_WEIGHT_CYM: f32 = 0.30;
const V2_WEIGHT_ART_CYM: f32 = 0.85;
const V2_WEIGHT_EMBLEM_CYM: f32 = 1.03;
const V2_WEIGHT_OSHI_LIFE_CYM: f32 = 0.632;

enum HashComponent {
    FullImageHash,
    FullImageCym,
    ArtBoxCym,
    EmblemCym,
    AltOshiLifeCym,
}

impl HashComponent {
    fn format_key(&self) -> &'static str {
        match self {
            HashComponent::FullImageHash => "H32",
            HashComponent::FullImageCym => "CYM",
            HashComponent::ArtBoxCym => "A_CYM",
            HashComponent::EmblemCym => "E_CYM",
            HashComponent::AltOshiLifeCym => "L_CYM",
        }
    }

    fn weight(&self) -> f32 {
        match self {
            HashComponent::FullImageHash => V2_WEIGHT_DG32,
            HashComponent::FullImageCym => V2_WEIGHT_CYM,
            HashComponent::ArtBoxCym => V2_WEIGHT_ART_CYM,
            HashComponent::EmblemCym => V2_WEIGHT_EMBLEM_CYM,
            HashComponent::AltOshiLifeCym => V2_WEIGHT_OSHI_LIFE_CYM,
        }
    }

    fn component_list(card_number: &str) -> Vec<Self> {
        let mut list = vec![HashComponent::FullImageHash, HashComponent::FullImageCym];

        // hBP01-007 - Hoshimachi Suisei, Exstreamer Cup Finals 2025 prizes
        if card_number == "hBP01-007" {
            list.push(HashComponent::EmblemCym);
            list.push(HashComponent::AltOshiLifeCym);
        }

        // hBP01-104 - Normal PC, Live Start Decks
        if card_number == "hBP01-104" {
            list.push(HashComponent::ArtBoxCym);
        }

        list
    }

    fn full_component_list() -> Vec<Self> {
        vec![
            HashComponent::FullImageHash,
            HashComponent::FullImageCym,
            HashComponent::ArtBoxCym,
            HashComponent::EmblemCym,
            HashComponent::AltOshiLifeCym,
        ]
    }

    fn prep_for_hash(img: &RgbImage) -> GrayImage {
        // use saturation for better gray
        let mut sat: image::ImageBuffer<image::Luma<u8>, Vec<u8>> = sat_gray_image(img);

        // blur the "sample" part of the image
        let start_x = 0;
        let end_x = img.width();
        let start_y = img.height() * 170 / 560;
        let end_y = img.height() * 340 / 560;
        let cropped =
            imageops::crop_imm(&sat, start_x, start_y, end_x - start_x, end_y - start_y).to_image();
        let blurred = imageops::fast_blur(&cropped, 20.0);
        imageops::overlay(&mut sat, &blurred, start_x as i64, start_y as i64);
        // sat.save("debug_sat.webp").ok();

        sat
    }

    fn to_hash_component(img: &GrayImage) -> String {
        let hasher32 = HasherConfig::new()
            .hash_size(V2_WIDTH_DG32, V2_WIDTH_DG32)
            .preproc_dct()
            .hash_alg(image_hasher::HashAlg::DoubleGradient)
            .to_hasher();
        let h32 = hasher32.hash_image(img);
        h32.to_base64()
    }

    fn calc_hash_component(
        &self,
        map1: &HashMap<&str, &str>,
        map2: &HashMap<&str, &str>,
        score: &mut f32,
        total_w: &mut f32,
    ) -> Option<()> {
        let key = self.format_key();
        let weight = self.weight();

        let a = ImageHash::<Box<[u8]>>::from_base64(map1.get(key)?).ok()?;
        let b = ImageHash::<Box<[u8]>>::from_base64(map2.get(key)?).ok()?;
        let bits = V2_BITS_DG32 as f32;
        let d = a.dist(&b) as f32 / bits; // normalized [0,1]
        if DEBUG {
            println!(
                "{V2_VERSION} {key} dist={d:.4} (bits={bits}) w={weight:.2} contrib={:.4}",
                d * weight
            );
        }
        *score += d * weight;
        *total_w += weight;
        Some(())
    }

    fn color_image_crop(&self, img: &RgbImage) -> RgbImage {
        let (start_x, start_y, end_x, end_y) = match self {
            HashComponent::FullImageCym => {
                return img.clone();
            }
            HashComponent::ArtBoxCym => (
                img.width() * 20 / 400,
                img.height() * 70 / 560,
                img.width() * 375 / 400,
                img.height() * 320 / 560,
            ),
            HashComponent::EmblemCym => (
                img.width() * 250 / 400,
                img.height() * 210 / 560,
                img.width() * 390 / 400,
                img.height() * 330 / 560,
            ),
            HashComponent::AltOshiLifeCym => (
                img.width() * 325 / 400,
                img.height() * 490 / 560,
                img.width() * 390 / 400,
                img.height() * 540 / 560,
            ),
            _ => unreachable!(),
        };

        let crop =
            imageops::crop_imm(img, start_x, start_y, end_x - start_x, end_y - start_y).to_image();
        if DEBUG {
            crop.save(format!("debug_crop_{}.webp", self.format_key()))
                .ok();
        }

        crop
    }

    fn to_color_component(img: RgbImage) -> String {
        // Average color (bias) component CYM=CCYYMM (hex)
        let (mut sum_c, mut sum_y, mut sum_m) = (0u64, 0u64, 0u64);
        for p in img.pixels() {
            let cymk = cymk(p.0);
            sum_c += (cymk.0 * 255.0) as u64;
            sum_y += (cymk.1 * 255.0) as u64;
            sum_m += (cymk.2 * 255.0) as u64;
        }
        let n = (img.width() as u64) * (img.height() as u64).max(1);
        let avg_c = (sum_c / n) as u8;
        let avg_y = (sum_y / n) as u8;
        let avg_m = (sum_m / n) as u8;
        let cym = format!("{:02X}{:02X}{:02X}", avg_c, avg_y, avg_m);
        cym
    }

    fn calc_color_component(
        &self,
        map1: &HashMap<&str, &str>,
        map2: &HashMap<&str, &str>,
        score: &mut f32,
        total_w: &mut f32,
    ) -> Option<()> {
        let key = self.format_key();
        let weight = self.weight();

        // Color bias (average color difference)
        let c1 = map1.get(key)?;
        let c2 = map2.get(key)?;
        if c1.len() == 6
            && c2.len() == 6
            && let (Ok(c1), Ok(y1), Ok(m1), Ok(c2), Ok(y2), Ok(m2)) = (
                u8::from_str_radix(&c1[0..2], 16),
                u8::from_str_radix(&c1[2..4], 16),
                u8::from_str_radix(&c1[4..6], 16),
                u8::from_str_radix(&c2[0..2], 16),
                u8::from_str_radix(&c2[2..4], 16),
                u8::from_str_radix(&c2[4..6], 16),
            )
        {
            let dc = (c1 as i16 - c2 as i16) as f32;
            let dy = (y1 as i16 - y2 as i16) as f32;
            let dm = (m1 as i16 - m2 as i16) as f32;
            let denom: f32 = (3.0f32 * 255.0f32 * 255.0f32).sqrt();
            let dcym = (dc * dc + dy * dy + dm * dm).sqrt() / denom; // [0,1]
            if DEBUG {
                println!(
                    "{V2_VERSION} {key} dist={dcym:.4} (denom={denom}) w={weight:.2} contrib={:.4}",
                    dcym * weight
                );
            }
            *score += dcym * weight;
            *total_w += weight;
        }
        Some(())
    }
}

pub fn path_to_image_hash(card_number: &str, path: &Path) -> String {
    let img = image::open(path).unwrap().into_rgb8();
    to_image_hash(card_number, &img)
}

pub fn to_image_hash(card_number: &str, img: &RgbImage) -> String {
    let prep = HashComponent::prep_for_hash(img);

    let comps = HashComponent::component_list(card_number);
    let map = comps
        .into_iter()
        .map(|c| {
            (
                c.format_key(),
                match c {
                    HashComponent::FullImageHash => HashComponent::to_hash_component(&prep),
                    cym @ (HashComponent::FullImageCym
                    | HashComponent::ArtBoxCym
                    | HashComponent::EmblemCym
                    | HashComponent::AltOshiLifeCym) => {
                        let crop = cym.color_image_crop(img);
                        HashComponent::to_color_component(crop)
                    }
                },
            )
        })
        .collect::<Vec<(_, _)>>();

    let hash = format_components(V2_VERSION, &map[..]);

    if DEBUG {
        println!("{hash}");
    }

    hash
}

pub fn can_merge(into: &CardIllustration, from: &CardIllustration, same_rarity: bool) -> bool {
    let dist = dist_hash(&into.img_hash, &from.img_hash);
    if DEBUG {
        println!(
            "can_merge({} {}, {} {}) = {dist}",
            into.card_number, into.rarity, from.card_number, from.rarity
        );
    }

    // if both have manage_id, they must be identical
    for language in [Language::Japanese, Language::English] {
        if into.manage_id.value(language).is_some()
            && from.manage_id.value(language).is_some()
            && dist != 0
        {
            return false;
        }
    }

    // more tolerance for manual cropping
    dist <= if same_rarity {
        DIST_TOLERANCE_SAME_RARITY
    } else {
        DIST_TOLERANCE_DIFF_RARITY
    }
}

pub fn dist_hash(h1: &str, h2: &str) -> u64 {
    (dist_hash_norm(h1, h2).unwrap_or(1.0) * 1000.0) as u64 // 1.0 is the minimum distance, so we scale it up
}

fn dist_hash_norm(h1: &str, h2: &str) -> Option<f32> {
    let map1 = parse_components(h1)?;
    let map2 = parse_components(h2)?;

    if map1.get(VERSION_COMPONENT)? != map2.get(VERSION_COMPONENT)? {
        return None;
    }

    let mut score = 0.0f32;
    let mut total_w = 0.0f32;

    // will automatically ignore missing components, so it's fine if some cards don't have all components
    for comp in HashComponent::full_component_list() {
        match comp {
            HashComponent::FullImageHash => {
                comp.calc_hash_component(&map1, &map2, &mut score, &mut total_w);
            }
            HashComponent::FullImageCym
            | HashComponent::ArtBoxCym
            | HashComponent::EmblemCym
            | HashComponent::AltOshiLifeCym => {
                comp.calc_color_component(&map1, &map2, &mut score, &mut total_w);
            }
        }
    }

    if total_w == 0.0 {
        return None;
    }

    let final_score = score;
    if DEBUG {
        println!("{V2_VERSION} distance score={final_score:.4}");
    }
    Some(final_score)
}

fn format_components(version: &str, map: &[(&str, String)]) -> String {
    version.to_owned() + "|" + &map.iter().map(|(k, v)| format!("{}={}", k, v)).join("|")
}

fn parse_components(s: &str) -> Option<HashMap<&str, &str>> {
    let mut m = HashMap::new();
    for (i, part) in s.split('|').enumerate() {
        if i == 0 {
            // starts with version
            m.insert(VERSION_COMPONENT, part);
        } else {
            let (k, v) = part.split_once('=')?;
            m.insert(k, v);
        }
    }
    Some(m)
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
