//! Fits the tables of CardHash on the card images, and writes `src/tables.rs` and
//! `src/basis.bin`:
//!
//! ```sh
//! cargo run --release -p hocg-fan-sim-assets-cardhash --example fit_tables -- <assets dir> [--dry-run] [--inputs <n>]
//! ```
//!
//! The images are the `.webp` files in `img` and `img_en` of the assets directory. The tables
//! are fitted on every other image first, to report how well they do on the others, then on
//! all of them.
//!
//! New tables make a new version of the format: change `VERSION` and the golden hash test.

use std::{
    env, fs,
    path::{Path, PathBuf},
    time::Instant,
};

use hocg_fan_sim_assets_cardhash::{
    BUDGET, COEFFICIENTS, Coefficients, Component, HEIGHT, KX, KY, Mean, Tables, Value, WIDTH,
    dequantize, oklab_image, quantize, render, srgb_to_oklab, target, transform, weight,
};
use nalgebra::DMatrix;
use rayon::prelude::*;

/// A value that is stored has at least this many bits.
const MIN_BITS: u8 = 2;
const MAX_BITS: u8 = 8;
/// Ranges tried for each value and number of bits, in standard deviations times the square
/// root of the bits.
const CLIPS: [f64; 9] = [0.6, 0.8, 1.0, 1.3, 1.6, 2.0, 2.5, 3.0, 4.0];
/// The coefficients the components are made of: the ones that vary the most. Fewer make a
/// smaller basis: 350 work as well as all of them, and 250 start to lose details.
const INPUTS: usize = 350;
/// Components that can get bits: the first ones, that explain the most.
const CANDIDATES: usize = 200;
/// Cards whose average color is at most this far from gray, for the scores.
const PALE: f64 = 0.025;

struct CardImage {
    coefs: Coefficients,
    /// what the hash approximates
    target: Vec<[f64; 3]>,
}

struct Fitted {
    mean: Box<Mean>,
    average: [Value; 3],
    inputs: Vec<u16>,
    components: Vec<Component>,
    basis: Vec<u8>,
}

impl Fitted {
    fn tables(&self) -> Tables<'_> {
        Tables {
            mean: &self.mean,
            average: &self.average,
            inputs: &self.inputs,
            components: &self.components,
            basis: &self.basis,
        }
    }
}

fn main() {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let Some(assets) = args.first().filter(|a| !a.starts_with("--")) else {
        eprintln!("usage: fit_tables <assets dir> [--dry-run] [--inputs <n>]");
        std::process::exit(2);
    };
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let inputs = args
        .iter()
        .position(|a| a == "--inputs")
        .and_then(|i| args.get(i + 1))
        .map_or(INPUTS, |n| n.parse().expect("a number of inputs"));

    let start = Instant::now();
    let files = image_files(Path::new(assets));
    let images = files
        .par_iter()
        .filter_map(|path| {
            let img = image::open(path)
                .inspect_err(|e| eprintln!("skipped {}: {e}", path.display()))
                .ok()?
                .into_rgb8();
            let target = target(img.as_raw(), img.width(), img.height());
            Some(CardImage {
                coefs: transform(&target),
                target,
            })
        })
        .collect::<Vec<_>>();
    eprintln!("{} images in {:.0?}", images.len(), start.elapsed());

    let (train, test): (Vec<_>, Vec<_>) = images.iter().enumerate().partition(|(i, _)| i % 2 == 0);
    let train = train.into_iter().map(|(_, img)| img).collect::<Vec<_>>();
    let test = test.into_iter().map(|(_, img)| img).collect::<Vec<_>>();
    let fitted = fit(&train, inputs);
    eprintln!(
        "fitted on {} images, on the {} others: {}",
        train.len(),
        test.len(),
        evaluate(fitted.tables(), &test)
    );

    let all = images.iter().collect::<Vec<_>>();
    let fitted = fit(&all, inputs);
    let component_bits = fitted
        .components
        .iter()
        .map(|c| c.value.bits as usize)
        .sum::<usize>();
    eprintln!(
        "fitted on all: average color bits {:?}, {} components ({component_bits} bits), basis {} KB",
        fitted.average.map(|v| v.bits),
        fitted.components.len(),
        fitted.basis.len() / 1024
    );
    eprintln!("on all: {}", evaluate(fitted.tables(), &all));

    if !dry_run {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        fs::write(dir.join("tables.rs"), tables_source(&fitted, images.len())).unwrap();
        fs::write(dir.join("basis.bin"), &fitted.basis).unwrap();
        eprintln!("wrote the tables in {}", dir.display());
    }
}

fn image_files(assets: &Path) -> Vec<PathBuf> {
    let mut files = ["img", "img_en"]
        .iter()
        .flat_map(|dir| walkdir::WalkDir::new(assets.join(dir)))
        .filter_map(Result::ok)
        .map(|e| e.into_path())
        .filter(|p| p.extension().is_some_and(|e| e == "webp"))
        .filter(|p| !p.components().any(|c| c.as_os_str() == "proxies"))
        .collect::<Vec<_>>();
    files.sort();
    files
}

/// The lowest and highest value of each Oklab channel over the 8-bit sRGB colors (they are
/// on the faces of the RGB cube).
fn srgb_gamut() -> [(f64, f64); 3] {
    let mut gamut = [(f64::MAX, f64::MIN); 3];
    for axis in 0..3 {
        for side in [0, 255] {
            for u in 0..=255u8 {
                for v in 0..=255u8 {
                    let mut rgb = [0; 3];
                    rgb[axis] = side;
                    rgb[(axis + 1) % 3] = u;
                    rgb[(axis + 2) % 3] = v;
                    for (c, x) in srgb_to_oklab(rgb).into_iter().enumerate() {
                        gamut[c] = (gamut[c].0.min(x), gamut[c].1.max(x));
                    }
                }
            }
        }
    }
    gamut
}

fn fit(images: &[&CardImage], inputs: usize) -> Fitted {
    let n = images.len() as f64;

    // the mean card, as it is stored
    let mut mean = Box::new([[[0.0f32; 3]; KX]; KY]);
    for (k, m) in mean
        .as_flattened_mut()
        .as_flattened_mut()
        .iter_mut()
        .enumerate()
    {
        let sum = images
            .iter()
            .map(|img| img.coefs.as_flattened().as_flattened()[k])
            .sum::<f64>();
        *m = (sum / n) as f32;
    }
    let no_values = [Value::new(0, 0.0); 3];
    let with_mean = Tables {
        mean: &mean,
        average: &no_values,
        inputs: &[],
        components: &[],
        basis: &[],
    };
    let devs = images
        .par_iter()
        .map(|img| with_mean.deviations(&img.coefs))
        .collect::<Vec<_>>();

    // the components: principal components of the coefficients that vary the most (not the
    // average color, which is stored apart)
    let variance = |k: usize| devs.iter().map(|d| d[k] * d[k]).sum::<f64>() / n;
    let mut input_list = (3..COEFFICIENTS).collect::<Vec<_>>();
    input_list.sort_by(|a, b| variance(*b).total_cmp(&variance(*a)));
    input_list.truncate(inputs);
    input_list.sort();
    let m = input_list.len();
    let x = DMatrix::from_fn(devs.len(), m, |r, c| devs[r][input_list[c]]);
    let covariance = x.transpose() * &x / n;
    let eigen = covariance.symmetric_eigen();
    let mut order = (0..m).collect::<Vec<_>>();
    order.sort_by(|a, b| eigen.eigenvalues[*b].total_cmp(&eigen.eigenvalues[*a]));
    order.truncate(CANDIDATES);
    // stored as signed bytes, with a scale for each component
    let mut basis = Vec::new();
    let mut candidates = Vec::new();
    for &e in &order {
        let v = eigen.eigenvectors.column(e);
        let scale = (v.amax() / 127.0) as f32;
        basis.extend(
            v.iter()
                .map(|x| (x / scale as f64).round().clamp(-127.0, 127.0) as i8 as u8),
        );
        candidates.push(Component::new(0, 0.0, scale));
    }
    let inputs = input_list.iter().map(|&k| k as u16).collect::<Vec<_>>();
    let all_candidates = Tables {
        mean: &mean,
        average: &no_values,
        inputs: &inputs,
        components: &candidates,
        basis: &basis,
    };
    let values = images
        .par_iter()
        .map(|img| all_candidates.values(&img.coefs))
        .collect::<Vec<_>>();

    // for each value (the average color, then the candidates) and number of bits: the error
    // and the best range. The values are weighted, so their errors are too
    let gamut = srgb_gamut();
    let stats = (0..3 + candidates.len())
        .into_par_iter()
        .map(|v| {
            let vals = values.iter().map(|x| x[v]).collect::<Vec<_>>();
            let variance = vals.iter().map(|x| x * x).sum::<f64>() / n;
            let sigma = variance.sqrt() + 1e-12;
            // the average color is never clipped, whatever the image: a preview can be off
            // in its details, not in its color
            let min_range = if v < 3 {
                let scale = ((WIDTH * HEIGHT) as f64).sqrt();
                let average = mean[0][0][v] as f64 / scale;
                let (low, high) = gamut[v];
                (average - low).max(high - average) * scale * weight(v)
            } else {
                0.0
            };
            let mut per_bits = vec![(variance, 0.0f32)];
            for bits in 1..=MAX_BITS {
                let best = CLIPS
                    .iter()
                    .map(|clip| {
                        let range = (clip * sigma * (bits as f64).sqrt()).max(min_range) as f32;
                        let r = range as f64;
                        let error = vals
                            .iter()
                            .map(|&x| (x - dequantize(quantize(x, bits, r), bits, r)).powi(2))
                            .sum::<f64>()
                            / n;
                        (error, range)
                    })
                    .min_by(|a, b| a.0.total_cmp(&b.0))
                    .unwrap();
                per_bits.push(best);
            }
            per_bits
        })
        .collect::<Vec<_>>();

    // bits go where they lower the error most per bit, looking at every number of bits a
    // value could go up to (a wide range pays off only with enough bits), and at least
    // MIN_BITS for a new value
    let mut bits = vec![0u8; stats.len()];
    let mut left = BUDGET;
    loop {
        let best = (0..stats.len())
            .flat_map(|k| {
                let have = bits[k];
                let first = if have == 0 { MIN_BITS } else { have + 1 };
                let per_bits = &stats[k];
                let current = per_bits[have as usize].0;
                (first..=MAX_BITS)
                    .filter(move |next| (next - have) as usize <= left)
                    .map(move |next| {
                        let gain = (current - per_bits[next as usize].0) / (next - have) as f64;
                        (k, next, gain)
                    })
            })
            .filter(|(_, _, gain)| *gain > 0.0)
            .max_by(|a, b| a.2.total_cmp(&b.2).then(b.0.cmp(&a.0)).then(b.1.cmp(&a.1)));
        let Some((k, next, _)) = best else { break };
        left -= (next - bits[k]) as usize;
        bits[k] = next;
    }

    let stored = |k: usize| Value::new(bits[k], stats[k][bits[k] as usize].1);
    let average = [stored(0), stored(1), stored(2)];
    assert!(
        average.iter().all(|v| v.bits > 0),
        "the average color has bits"
    );
    let mut components = Vec::new();
    let mut kept_basis = Vec::new();
    for (k, candidate) in candidates.iter().enumerate() {
        if bits[3 + k] > 0 {
            let value = stored(3 + k);
            components.push(Component::new(value.bits, value.range, candidate.scale));
            kept_basis.extend_from_slice(&basis[k * m..][..m]);
        }
    }
    Fitted {
        mean,
        average,
        inputs,
        components,
        basis: kept_basis,
    }
}

/// Scores of decoded hashes against what they approximate, x100, averaged over images.
#[derive(Debug, Default, Clone, Copy)]
struct Scores {
    /// Oklab distance, color weighted
    error: f64,
    /// lightness offset of what is shown (clamped to sRGB)
    bias: f64,
    /// lightness only
    lightness: f64,
    /// color (a and b) only
    color: f64,
    /// color only, on pale cards (where a color error is most visible)
    pale_color: f64,
    /// largest color error of a pixel, on pale cards
    pale_worst: f64,
    pale: usize,
}

impl std::fmt::Display for Scores {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "error {:.2}, lightness bias {:+.2}, lightness {:.2}, color {:.2}, on {} pale cards: color {:.2}, worst pixel {:.2}",
            self.error,
            self.bias,
            self.lightness,
            self.color,
            self.pale,
            self.pale_color,
            self.pale_worst
        )
    }
}

fn evaluate(tables: Tables, images: &[&CardImage]) -> Scores {
    let all = images
        .par_iter()
        .map(|img| {
            let coefs = tables.decode(&tables.encode(&img.coefs)).unwrap();
            let decoded = oklab_image(&coefs, WIDTH as u32, HEIGHT as u32);
            let pixels = img.target.len() as f64;
            let mean = |f: &dyn Fn(&[f64; 3], &[f64; 3]) -> f64| {
                decoded
                    .iter()
                    .zip(&img.target)
                    .map(|(d, t)| f(d, t))
                    .sum::<f64>()
                    / pixels
            };
            let error = mean(&|d, t| (0..3).map(|c| ((d[c] - t[c]) * weight(c)).powi(2)).sum());
            let lightness = mean(&|d, t| (d[0] - t[0]).powi(2));
            let color = mean(&|d, t| (d[1] - t[1]).powi(2) + (d[2] - t[2]).powi(2));
            let worst = decoded
                .iter()
                .zip(&img.target)
                .map(|(d, t)| ((d[1] - t[1]).powi(2) + (d[2] - t[2]).powi(2)).sqrt())
                .fold(0.0, f64::max);
            let pale = mean(&|_, t| (t[1] * t[1] + t[2] * t[2]).sqrt()) < PALE;
            let shown = render(&coefs, WIDTH as u32, HEIGHT as u32);
            let bias = shown
                .chunks(4)
                .zip(&img.target)
                .map(|(px, t)| srgb_to_oklab([px[0], px[1], px[2]])[0] - t[0])
                .sum::<f64>()
                / pixels;
            Scores {
                error: error.sqrt() * 100.0,
                bias: bias * 100.0,
                lightness: lightness.sqrt() * 100.0,
                color: color.sqrt() * 100.0,
                pale_color: if pale { color.sqrt() * 100.0 } else { 0.0 },
                pale_worst: if pale { worst * 100.0 } else { 0.0 },
                pale: pale as usize,
            }
        })
        .collect::<Vec<_>>();
    let n = all.len() as f64;
    let pale = all.iter().map(|s| s.pale).sum::<usize>();
    let average = |f: fn(&Scores) -> f64, count: f64| all.iter().map(f).sum::<f64>() / count;
    Scores {
        error: average(|s| s.error, n),
        bias: average(|s| s.bias, n),
        lightness: average(|s| s.lightness, n),
        color: average(|s| s.color, n),
        pale_color: average(|s| s.pale_color, pale.max(1) as f64),
        pale_worst: average(|s| s.pale_worst, pale.max(1) as f64),
        pale,
    }
}

fn tables_source(fitted: &Fitted, images: usize) -> String {
    let mut out = format!(
        "// Made by `cargo run --release -p hocg-fan-sim-assets-cardhash --example fit_tables` from\n\
         // {images} card images. Part of the format: see the crate documentation.\n\
         use crate::{{Component, Mean, Value}};\n\n\
         /// The mean card, by frequency down, across, and Oklab channel.\n\
         pub(crate) static MEAN: Mean = [\n"
    );
    for row in fitted.mean.iter() {
        let cells = row
            .iter()
            .map(|c| format!("[{:?}, {:?}, {:?}]", c[0], c[1], c[2]))
            .collect::<Vec<_>>();
        out += &format!("    [{}],\n", cells.join(", "));
    }
    out += "];\n\n/// The average color (L, a, b).\npub(crate) static AVERAGE: [Value; 3] = [\n";
    for v in &fitted.average {
        out += &format!("    Value::new({}, {:?}),\n", v.bits, v.range);
    }
    out += "];\n\n/// The coefficients the components are made of.\npub(crate) static INPUTS: &[u16] = &[\n";
    for chunk in fitted.inputs.chunks(24) {
        let line = chunk.iter().map(|i| i.to_string()).collect::<Vec<_>>();
        out += &format!("    {},\n", line.join(", "));
    }
    out += "];\n\n/// The components, in the order they are written: bits, range, scale of the basis row.\n\
            pub(crate) static COMPONENTS: &[Component] = &[\n";
    for c in &fitted.components {
        out += &format!(
            "    Component::new({}, {:?}, {:?}),\n",
            c.value.bits, c.value.range, c.scale
        );
    }
    out += "];\n\n/// A row of `INPUTS.len()` signed bytes for each component.\n\
            pub(crate) static BASIS: &[u8] = include_bytes!(\"basis.bin\");\n";
    out
}
