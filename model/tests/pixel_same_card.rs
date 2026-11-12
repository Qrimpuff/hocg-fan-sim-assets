use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use hocg_fan_sim_assets_model::{
    CardIllustration, CardsDatabase, Language,
    img_hash::{dist_hash, is_similar, path_to_image_hash},
};
use image::{RgbImage, imageops::FilterType};

use super::pixel_reviews::{resolve_asset, sha256_file};

const WIDTH: usize = 128;
const HEIGHT: usize = 180;
const SHIFT: i32 = 4;
const WINDOW_W: usize = 8;
const WINDOW_H: usize = 8;
// Same-language pairs: identical art differs only by rarity stamps, foil and scans.
const SAME_LANG_SAME_MEAN: f64 = 3.0;
const SAME_LANG_SAME_WINDOW: f64 = 25.0;
const SAME_LANG_DIFFERENT_MEAN: f64 = 15.0;
// Japanese/English pairs: translated text alone causes large local differences, so
// small art changes such as emblems cannot be ruled out; these are less reliable.
const CROSS_LANG_SAME_MEAN: f64 = 10.0;
const CROSS_LANG_SAME_WINDOW: f64 = 100.0;
const CROSS_LANG_DIFFERENT_MEAN: f64 = 20.0;
const REPORT_HEADER: &str = "pixel_verdict\tmean_abs\twindow_max\tcolor_mean\tcolor_window_max\tdx\tdy\tcard_number\trarity_a\trarity_b\tlanguage_a\tlanguage_b\tsame_language\tsame_illustration\tstored_same\thash_dist\thash_similar\tpath_a\tpath_b";

struct Pixels {
    rgb: RgbImage,
    gray: Vec<u8>,
}

struct PairStats {
    mean_abs: f64,
    window_max: f64,
    color_mean: f64,
    color_window_max: f64,
    dx: i32,
    dy: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PixelVerdict {
    Same,
    Different,
    Dubious,
}

impl PixelVerdict {
    fn as_str(self) -> &'static str {
        match self {
            PixelVerdict::Same => "same",
            PixelVerdict::Different => "different",
            PixelVerdict::Dubious => "dubious",
        }
    }
}

impl PairStats {
    // A suggestion for manual review, not ground truth.
    fn verdict(&self, same_language: bool) -> PixelVerdict {
        let mean = self.mean_abs.max(self.color_mean);
        let window = self.window_max.max(self.color_window_max);
        let (same_mean, same_window, different_mean) = if same_language {
            (
                SAME_LANG_SAME_MEAN,
                SAME_LANG_SAME_WINDOW,
                SAME_LANG_DIFFERENT_MEAN,
            )
        } else {
            (
                CROSS_LANG_SAME_MEAN,
                CROSS_LANG_SAME_WINDOW,
                CROSS_LANG_DIFFERENT_MEAN,
            )
        };
        if mean < same_mean && window < same_window {
            PixelVerdict::Same
        } else if mean >= different_mean {
            PixelVerdict::Different
        } else {
            PixelVerdict::Dubious
        }
    }
}

fn load_pixels(path: &Path) -> Pixels {
    let img = image::open(path)
        .unwrap_or_else(|e| panic!("failed to open {}: {}", path.display(), e))
        .resize_exact(WIDTH as u32, HEIGHT as u32, FilterType::Triangle);
    Pixels {
        rgb: img.to_rgb8(),
        gray: img.to_luma8().into_raw(),
    }
}

fn shifted_index(x: usize, y: usize, dx: i32, dy: i32) -> usize {
    let x = (x as i32 + dx).clamp(0, WIDTH as i32 - 1) as usize;
    let y = (y as i32 + dy).clamp(0, HEIGHT as i32 - 1) as usize;
    y * WIDTH + x
}

fn max_window(diff: &[u32]) -> f64 {
    let stride = WIDTH + 1;
    let mut integral = vec![0u64; stride * (HEIGHT + 1)];
    for y in 0..HEIGHT {
        let mut row = 0u64;
        for x in 0..WIDTH {
            row += u64::from(diff[y * WIDTH + x]);
            integral[(y + 1) * stride + x + 1] = integral[y * stride + x + 1] + row;
        }
    }
    let mut max = 0;
    for y in 0..=HEIGHT - WINDOW_H {
        for x in 0..=WIDTH - WINDOW_W {
            let bottom_right = integral[(y + WINDOW_H) * stride + x + WINDOW_W];
            let top_right = integral[y * stride + x + WINDOW_W];
            let bottom_left = integral[(y + WINDOW_H) * stride + x];
            let top_left = integral[y * stride + x];
            max = max.max(bottom_right + top_left - top_right - bottom_left);
        }
    }
    max as f64 / (WINDOW_W * WINDOW_H) as f64
}

// Compare in both directions so an alignment cannot hide a change at an edge.
fn best_shift(a: &Pixels, b: &Pixels) -> PairStats {
    let mut best = (u64::MAX, 0, 0);
    for dy in -SHIFT..=SHIFT {
        for dx in -SHIFT..=SHIFT {
            let mut sum = 0u64;
            for y in 0..HEIGHT {
                for x in 0..WIDTH {
                    let va = a.gray[y * WIDTH + x];
                    let vb = b.gray[shifted_index(x, y, dx, dy)];
                    sum += u64::from(va.abs_diff(vb));
                    let va = a.gray[shifted_index(x, y, -dx, -dy)];
                    let vb = b.gray[y * WIDTH + x];
                    sum += u64::from(va.abs_diff(vb));
                }
            }
            if sum < best.0 {
                best = (sum, dx, dy);
            }
        }
    }

    let (sum, dx, dy) = best;
    let mut gray_diff = Vec::with_capacity(WIDTH * HEIGHT);
    let mut color_diff = Vec::with_capacity(WIDTH * HEIGHT);
    let mut reverse_gray_diff = Vec::with_capacity(WIDTH * HEIGHT);
    let mut reverse_color_diff = Vec::with_capacity(WIDTH * HEIGHT);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let index = y * WIDTH + x;
            let other = shifted_index(x, y, dx, dy);
            gray_diff.push(u32::from(a.gray[index].abs_diff(b.gray[other])));
            let reverse = shifted_index(x, y, -dx, -dy);
            reverse_gray_diff.push(u32::from(a.gray[reverse].abs_diff(b.gray[index])));
            let rgb_a = &a.rgb.as_raw()[index * 3..index * 3 + 3];
            let rgb_b = &b.rgb.as_raw()[other * 3..other * 3 + 3];
            color_diff.push(
                rgb_a
                    .iter()
                    .zip(rgb_b)
                    .map(|(a, b)| u32::from(a.abs_diff(*b)))
                    .sum::<u32>(),
            );
            let rgb_a = &a.rgb.as_raw()[reverse * 3..reverse * 3 + 3];
            let rgb_b = &b.rgb.as_raw()[index * 3..index * 3 + 3];
            reverse_color_diff.push(
                rgb_a
                    .iter()
                    .zip(rgb_b)
                    .map(|(a, b)| u32::from(a.abs_diff(*b)))
                    .sum::<u32>(),
            );
        }
    }
    PairStats {
        mean_abs: sum as f64 / (WIDTH * HEIGHT * 2) as f64,
        window_max: max_window(&gray_diff).max(max_window(&reverse_gray_diff)),
        color_mean: color_diff
            .iter()
            .chain(&reverse_color_diff)
            .map(|d| u64::from(*d))
            .sum::<u64>() as f64
            / (WIDTH * HEIGHT * 6) as f64,
        color_window_max: max_window(&color_diff).max(max_window(&reverse_color_diff)) / 3.0,
        dx,
        dy,
    }
}

struct ImageEntry {
    illustration: CardIllustration,
    illustration_index: usize,
    path: PathBuf,
    language: &'static str,
}

fn is_proxy_image(path: &str) -> bool {
    path.split(['/', '\\'])
        .find(|part| !part.is_empty() && *part != ".")
        .is_some_and(|part| part.eq_ignore_ascii_case("proxies"))
}

fn render_pixel_html(header: &str, lines: &[String], assets_path: &Path) -> String {
    let columns: Vec<&str> = header.split('\t').collect();
    assert_eq!(header, REPORT_HEADER, "unexpected pixel report columns");
    let rows: Vec<Vec<&str>> = lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let row: Vec<&str> = line.split('\t').collect();
            assert_eq!(
                row.len(),
                columns.len(),
                "invalid pixel report row {}",
                index + 2
            );
            row
        })
        .collect();
    let root = assets_path
        .canonicalize()
        .unwrap_or_else(|error| panic!("failed to resolve {}: {error}", assets_path.display()));
    let mut fingerprints = HashMap::new();
    for row in &rows {
        for column in [17, 18] {
            let path = row[column].replace('\\', "/");
            fingerprints.entry(path).or_insert_with_key(|path| {
                let resolved = resolve_asset(&root, path).unwrap_or_else(|error| panic!("{error}"));
                sha256_file(&resolved).unwrap_or_else(|error| panic!("{error}"))
            });
        }
    }
    let root = root.to_str().expect("assets path is not valid UTF-8");
    let data = serde_json::json!({
        "assetsRoot": root,
        "columns": columns,
        "rows": rows,
        "fingerprints": fingerprints,
    })
    .to_string()
    .replace('<', "\\u003c")
    .replace('>', "\\u003e")
    .replace('&', "\\u0026");
    include_str!("pixel_report.html").replace("<!-- PIXEL_REPORT_DATA -->", &data)
}

fn write_pixel_html(path: &str, header: &str, lines: &[String], assets_path: &Path) {
    let html = render_pixel_html(header, lines, assets_path);
    fs::write(path, html).unwrap_or_else(|error| panic!("failed to write {path}: {error}"));
    println!("wrote {path}");
}

// Suggests same/different/dubious for every pair of images of a card. The
// suggestions are only a starting point for manual review in the HTML report.
#[ignore]
#[test]
fn test_pixel_same_card_pairs() {
    let assets_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("assets");
    let cards_json_path = assets_path.join("hocg_cards.json");
    let s = fs::read_to_string(&cards_json_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", cards_json_path.display(), e));
    let all_cards: CardsDatabase = serde_json::from_str(&s).unwrap();
    let mut cache: HashMap<PathBuf, Pixels> = HashMap::new();
    let mut hash_disagreements = 0;
    let mut rows: Vec<(String, PixelVerdict, f64, String)> = Vec::new();
    let mut missing = Vec::new();
    let mut skipped_proxies = 0;

    for card in all_cards.values() {
        let mut images = Vec::new();
        for (illustration_index, illust) in card.illustrations.iter().enumerate() {
            for (language, dir, label) in [
                (Language::Japanese, "img", "JP"),
                (Language::English, "img_en", "EN"),
            ] {
                let Some(path) = illust.img_path.value(language) else {
                    continue;
                };
                if is_proxy_image(path) {
                    skipped_proxies += 1;
                    continue;
                }
                let path = assets_path.join(dir).join(path);
                if !path.is_file() {
                    missing.push(path);
                    continue;
                }
                cache
                    .entry(path.clone())
                    .or_insert_with(|| load_pixels(&path));
                let mut illustration = illust.clone();
                illustration.img_hash = path_to_image_hash(&path, Some(language));
                images.push(ImageEntry {
                    illustration,
                    illustration_index,
                    path,
                    language: label,
                });
            }
        }

        for x in 0..images.len() {
            for y in x + 1..images.len() {
                let a = &images[x];
                let b = &images[y];
                let stats = best_shift(&cache[&a.path], &cache[&b.path]);
                let hash_similar = is_similar(&a.illustration, &b.illustration);
                let same_illustration = a.illustration_index == b.illustration_index;
                let same_language = a.language == b.language;
                let verdict = stats.verdict(same_language);
                if matches!(
                    (verdict, hash_similar),
                    (PixelVerdict::Same, false) | (PixelVerdict::Different, true)
                ) {
                    hash_disagreements += 1;
                }
                let line = format!(
                    "{}\t{:.2}\t{:.2}\t{:.2}\t{:.2}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    verdict.as_str(),
                    stats.mean_abs,
                    stats.window_max,
                    stats.color_mean,
                    stats.color_window_max,
                    stats.dx,
                    stats.dy,
                    a.illustration.card_number,
                    a.illustration.rarity,
                    b.illustration.rarity,
                    a.language,
                    b.language,
                    same_language,
                    same_illustration,
                    if a.illustration.similarity_index == 0 || b.illustration.similarity_index == 0
                    {
                        "unset"
                    } else if a.illustration.similarity_index == b.illustration.similarity_index {
                        "true"
                    } else {
                        "false"
                    },
                    dist_hash(&a.illustration.img_hash, &b.illustration.img_hash),
                    hash_similar,
                    a.path
                        .strip_prefix(&assets_path)
                        .unwrap_or(&a.path)
                        .display(),
                    b.path
                        .strip_prefix(&assets_path)
                        .unwrap_or(&b.path)
                        .display(),
                );
                rows.push((
                    a.illustration.card_number.clone(),
                    verdict,
                    stats.window_max,
                    line,
                ));
            }
        }
    }

    rows.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| b.2.total_cmp(&a.2))
            .then_with(|| a.3.cmp(&b.3))
    });
    let count = |verdict| rows.iter().filter(|row| row.1 == verdict).count();
    println!(
        "pairs={} same={} different={} dubious={} pixel_hash_disagreements={} skipped_proxies={} missing_images={}",
        rows.len(),
        count(PixelVerdict::Same),
        count(PixelVerdict::Different),
        count(PixelVerdict::Dubious),
        hash_disagreements,
        skipped_proxies,
        missing.len()
    );
    for path in &missing {
        eprintln!("missing image: {}", path.display());
    }
    let tsv_path = std::env::var("PIXEL_OUT").ok();
    let html_path = std::env::var("PIXEL_HTML_OUT").ok();
    if let Some(out_path) = &tsv_path {
        let mut out = fs::File::create(out_path)
            .unwrap_or_else(|e| panic!("failed to create {out_path}: {e}"));
        writeln!(out, "{REPORT_HEADER}").unwrap();
        for (_, _, _, line) in &rows {
            writeln!(out, "{line}").unwrap();
        }
        println!("wrote {out_path}");
    }
    if let Some(out_path) = &html_path {
        let lines: Vec<String> = rows.iter().map(|(_, _, _, line)| line.clone()).collect();
        write_pixel_html(out_path, REPORT_HEADER, &lines, &assets_path);
    }
    if tsv_path.is_none() && html_path.is_none() {
        println!("{REPORT_HEADER}");
        for (_, _, _, line) in rows
            .iter()
            .filter(|row| row.1 == PixelVerdict::Dubious)
            .take(30)
        {
            println!("{line}");
        }
        println!("Set PIXEL_OUT or PIXEL_HTML_OUT to write the complete report.");
    }
}

#[ignore]
#[test]
fn test_pixel_html_from_tsv() {
    let input = std::env::var("PIXEL_IN").expect("set PIXEL_IN to an existing TSV report");
    let output =
        std::env::var("PIXEL_HTML_OUT").expect("set PIXEL_HTML_OUT to an HTML output path");
    let contents = fs::read_to_string(&input)
        .unwrap_or_else(|error| panic!("failed to read {input}: {error}"));
    let mut lines = contents.lines();
    let header = lines.next().expect("pixel report is empty");
    let lines: Vec<String> = lines.map(str::to_owned).collect();
    let assets_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("assets");
    write_pixel_html(&output, header, &lines, &assets_path);
}

#[test]
fn html_report_escapes_embedded_data() {
    let assets = tempfile::tempdir().unwrap();
    fs::create_dir(assets.path().join("img")).unwrap();
    fs::write(assets.path().join("img").join("example.webp"), b"fixture").unwrap();
    let mut row = vec![""; REPORT_HEADER.split('\t').count()];
    row[7] = "</script><script>alert('x')</script>";
    row[17] = "img/example.webp";
    row[18] = "img/example.webp";
    let html = render_pixel_html(REPORT_HEADER, &[row.join("\t")], assets.path());
    assert!(!html.contains("</script><script>alert"));
    assert!(html.contains("\\u003c/script\\u003e"));
}

#[test]
fn excludes_proxy_image_paths() {
    for path in [
        "proxies/hBP09/hBP09-105_U.webp",
        "proxies\\hBP09\\hBP09-105_U.webp",
        "./Proxies/hBP09/hBP09-105_U.webp",
    ] {
        assert!(is_proxy_image(path), "{path}");
    }
    for path in [
        "hBP09/hBP09-105_U.webp",
        "hBP09/EN_hBP09-105_U.webp",
        "proxies_extra/card.webp",
        "hBP09/proxies/card.webp",
    ] {
        assert!(!is_proxy_image(path), "{path}");
    }
}

#[test]
fn pixel_verdict_never_calls_known_differences_same() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("card_images");
    for (a, b) in [
        ("hBP09-105_U.webp", "hBP09-105_P.webp"),
        ("hBP05-080_U.webp", "hBP05-080_P.webp"),
        ("hBP01-007_P_a.webp", "hBP01-007_P_b.webp"),
        ("hBP03-040_C_a.webp", "hBP03-040_C_b.webp"),
        ("hBP03-037_C_a.webp", "hBP03-037_C_b.webp"),
        ("hBP01-088_C_a.webp", "hBP01-088_C_b.webp"),
    ] {
        let stats = best_shift(&load_pixels(&dir.join(a)), &load_pixels(&dir.join(b)));
        assert_ne!(
            stats.verdict(true),
            PixelVerdict::Same,
            "{a} vs {b}: mean={}, window={}",
            stats.mean_abs,
            stats.window_max
        );
    }
    let image = load_pixels(&dir.join("hBP09-105_U.webp"));
    assert_eq!(best_shift(&image, &image).verdict(true), PixelVerdict::Same);
    assert_eq!(
        best_shift(&image, &image).verdict(false),
        PixelVerdict::Same
    );
}

#[test]
fn pixel_verdict_flags_small_local_changes_as_dubious() {
    let rgb = RgbImage::from_pixel(WIDTH as u32, HEIGHT as u32, image::Rgb([0, 0, 0]));
    let a = Pixels {
        gray: vec![0; WIDTH * HEIGHT],
        rgb,
    };
    for (x, y) in [(6, 6), (0, 0), (WIDTH - 4, HEIGHT - 4)] {
        let mut rgb = a.rgb.clone();
        for yy in y..y + 4 {
            for xx in x..x + 4 {
                rgb.put_pixel(xx as u32, yy as u32, image::Rgb([255, 255, 255]));
            }
        }

        let b = Pixels {
            gray: image::DynamicImage::ImageRgb8(rgb.clone())
                .to_luma8()
                .into_raw(),
            rgb,
        };
        assert_eq!(
            best_shift(&a, &b).verdict(true),
            PixelVerdict::Dubious,
            "difference at {x},{y}"
        );
    }
}

#[test]
fn pixel_verdict_detects_color_changes_with_similar_luminance() {
    let a = RgbImage::from_pixel(WIDTH as u32, HEIGHT as u32, image::Rgb([255, 0, 0]));
    let b = RgbImage::from_pixel(WIDTH as u32, HEIGHT as u32, image::Rgb([0, 76, 0]));
    let pixels = |rgb: RgbImage| Pixels {
        gray: image::DynamicImage::ImageRgb8(rgb.clone())
            .to_luma8()
            .into_raw(),
        rgb,
    };
    let stats = best_shift(&pixels(a), &pixels(b));
    assert!(stats.mean_abs < SAME_LANG_SAME_MEAN);
    assert_eq!(stats.verdict(true), PixelVerdict::Different);
}
