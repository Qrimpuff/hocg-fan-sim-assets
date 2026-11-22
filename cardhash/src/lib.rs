//! CardHash: a 54-character string that decodes to a blurred preview of a card image, to show
//! while the image loads.
//!
//! It works like [BlurHash](https://blurha.sh), but is made for cards only, which makes it
//! smaller and closer to the image:
//!
//! - The card is shrunk to 40x56 and blurred in linear light (as the eye mixes colors, so
//!   that small bright details such as text do not make the preview darker), then converted
//!   to [Oklab](https://bottosson.github.io/posts/oklab/).
//! - Its low frequencies (a DCT) are taken as their difference from the mean card, so what
//!   every card has (the frame, the text box) costs nothing.
//! - The average color is stored as is, over the whole sRGB gamut: it is never off.
//! - The rest is stored as components of a basis learned from the card images (their
//!   principal components): each is a way cards differ from the mean card, so a few of them
//!   describe a card much better than as many frequencies.
//! - Each value has its own number of bits and range, also fitted on the card images (see
//!   `examples/fit_tables.rs`).
//! - There is no header: every hash has the same size and layout. The bits are written in
//!   URL-safe base64 after a version character, [`VERSION`].
//!
//! The tables are part of the format: a hash can only be decoded with the tables it was made
//! with. Fitting them again makes a new version.

#[rustfmt::skip]
mod tables;

use std::sync::OnceLock;

/// The first character of every hash, changed when the format or its tables change.
pub const VERSION: char = '1';
/// Length of a hash, [`VERSION`] included.
pub const HASH_LEN: usize = 54;

/// Size of the image a hash describes. A decoded hash is this image resampled.
#[doc(hidden)]
pub const WIDTH: usize = 40;
#[doc(hidden)]
pub const HEIGHT: usize = 56;
/// Frequencies of the image that a hash describes, across and down.
#[doc(hidden)]
pub const KX: usize = 14;
#[doc(hidden)]
pub const KY: usize = 20;
/// Number of [`Coefficients`], flattened (`(j * KX + i) * 3 + c`).
#[doc(hidden)]
pub const COEFFICIENTS: usize = KY * KX * 3;
/// Bits stored in a hash, after the version.
#[doc(hidden)]
pub const BUDGET: usize = (HASH_LEN - 1) * 6;
/// How much more an error on the color (Oklab a and b) counts than on the lightness: the
/// color coefficients are multiplied by it before they are stored.
#[doc(hidden)]
pub const CHROMA_WEIGHT: f64 = 2.0;
/// Standard deviation of the blur, in pixels of the 40x56 image.
const BLUR: f64 = 2.5;

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// DCT coefficients of an image in Oklab, by frequency down, frequency across, and channel.
#[doc(hidden)]
pub type Coefficients = [[[f64; 3]; KX]; KY];
/// The mean card: [`Coefficients`] that hashes store the difference from.
#[doc(hidden)]
pub type Mean = [[[f32; 3]; KX]; KY];

/// A value stored in a hash, with `bits` bits over `-range..range`.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Value {
    pub bits: u8,
    pub range: f32,
}

impl Value {
    pub const fn new(bits: u8, range: f32) -> Self {
        Self { bits, range }
    }
}

/// A component of the basis: how it is stored, and the scale of its row of [`Tables::basis`].
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Component {
    pub value: Value,
    pub scale: f32,
}

impl Component {
    pub const fn new(bits: u8, range: f32, scale: f32) -> Self {
        Self {
            value: Value::new(bits, range),
            scale,
        }
    }
}

/// The tables of a version of the format. The values of a hash are the average color, then
/// the components, all in weighted (see [`CHROMA_WEIGHT`]) differences from the mean card.
#[doc(hidden)]
#[derive(Debug, Clone, Copy)]
pub struct Tables<'a> {
    pub mean: &'a Mean,
    /// how the average color (L, a, b) is stored
    pub average: &'a [Value; 3],
    /// the coefficients the components are made of (flattened indices, not the average color)
    pub inputs: &'a [u16],
    pub components: &'a [Component],
    /// the components, a row of `inputs.len()` signed bytes each, times their scale
    pub basis: &'a [u8],
}

const TABLES: Tables<'static> = Tables {
    mean: &tables::MEAN,
    average: &tables::AVERAGE,
    inputs: tables::INPUTS,
    components: tables::COMPONENTS,
    basis: tables::BASIS,
};

/// The tables of this version.
#[doc(hidden)]
pub fn current_tables() -> Tables<'static> {
    TABLES
}

/// The hash of a card image, given as 8-bit sRGB pixels (`RGBRGB...`, row by row).
///
/// # Panics
///
/// If the image is empty, or `rgb` does not have `width * height * 3` bytes.
pub fn encode(rgb: &[u8], width: u32, height: u32) -> String {
    TABLES.encode(&coefficients(rgb, width, height))
}

/// Decodes a hash to an image of the given size, as 8-bit sRGB pixels with alpha
/// (`RGBARGBA...`, row by row, always opaque). The preview is smooth: a small image (such as
/// 20x28) stretched by the browser looks the same as a large one.
///
/// `None` if the hash was not made by this version of the format.
pub fn decode(hash: &str, width: u32, height: u32) -> Option<Vec<u8>> {
    Some(render(&TABLES.decode(hash)?, width, height))
}

/// The weight of a flattened coefficient (see [`CHROMA_WEIGHT`]).
#[doc(hidden)]
pub fn weight(k: usize) -> f64 {
    if k.is_multiple_of(3) { 1.0 } else { CHROMA_WEIGHT }
}

impl Tables<'_> {
    /// The weighted differences of coefficients from the mean card, flattened.
    pub fn deviations(&self, coefs: &Coefficients) -> Vec<f64> {
        let mean = self.mean.as_flattened().as_flattened();
        coefs
            .as_flattened()
            .as_flattened()
            .iter()
            .zip(mean)
            .enumerate()
            .map(|(k, (x, m))| (x - *m as f64) * weight(k))
            .collect()
    }

    fn row(&self, component: usize) -> impl Iterator<Item = (usize, f64)> + '_ {
        let n = self.inputs.len();
        let scale = self.components[component].scale as f64;
        self.basis[component * n..][..n]
            .iter()
            .zip(self.inputs)
            .map(move |(b, i)| (*i as usize, *b as i8 as f64 * scale))
    }

    /// The values a hash stores of an image (before they are quantized): the average color,
    /// then the components.
    pub fn values(&self, coefs: &Coefficients) -> Vec<f64> {
        let dev = self.deviations(coefs);
        let components =
            (0..self.components.len()).map(|k| self.row(k).map(|(i, b)| b * dev[i]).sum::<f64>());
        dev[..3].iter().copied().chain(components).collect()
    }

    fn stored(&self) -> impl Iterator<Item = &Value> {
        self.average
            .iter()
            .chain(self.components.iter().map(|c| &c.value))
    }

    /// The hash of the coefficients of an image.
    pub fn encode(&self, coefs: &Coefficients) -> String {
        let mut out = BitWriter::default();
        for (stored, v) in self.stored().zip(self.values(coefs)) {
            out.push(quantize(v, stored.bits, stored.range as f64), stored.bits);
        }
        out.finish()
    }

    /// The coefficients stored in a hash, `None` if it is not a hash of this version.
    pub fn decode(&self, hash: &str) -> Option<Coefficients> {
        let data = hash.strip_prefix(VERSION)?.as_bytes();
        if data.len() != HASH_LEN - 1 || !data.iter().all(|c| BASE64.contains(c)) {
            return None;
        }
        let mut bits = BitReader::new(data);
        let mut values = self
            .stored()
            .map(|v| dequantize(bits.next(v.bits), v.bits, v.range as f64))
            .collect::<Vec<_>>()
            .into_iter();
        let mut dev = [0.0; COEFFICIENTS];
        for d in &mut dev[..3] {
            *d = values.next()?;
        }
        for (k, v) in values.enumerate() {
            for (i, b) in self.row(k) {
                dev[i] += v * b;
            }
        }
        let mut coefs = [[[0.0; 3]; KX]; KY];
        let mean = self.mean.as_flattened().as_flattened();
        for (k, x) in coefs
            .as_flattened_mut()
            .as_flattened_mut()
            .iter_mut()
            .enumerate()
        {
            *x = mean[k] as f64 + dev[k] / weight(k);
        }
        Some(coefs)
    }
}

/// Bits written in base64, after the version.
#[derive(Default)]
struct BitWriter {
    out: String,
    acc: u32,
    n: u8,
}

impl BitWriter {
    fn push(&mut self, value: u32, bits: u8) {
        if self.out.is_empty() {
            self.out.push(VERSION);
        }
        self.acc = (self.acc << bits | value) & 0xff_ffff;
        self.n += bits;
        while self.n >= 6 {
            self.n -= 6;
            self.out
                .push(BASE64[(self.acc >> self.n) as usize & 63] as char);
        }
    }

    fn finish(mut self) -> String {
        if self.n > 0 {
            self.push(0, 6 - self.n);
        }
        while self.out.len() < HASH_LEN {
            self.out.push(BASE64[0] as char);
        }
        self.out
    }
}

/// Bits read from base64 characters (all valid).
struct BitReader<'a> {
    chars: std::slice::Iter<'a, u8>,
    acc: u32,
    n: u8,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            chars: data.iter(),
            acc: 0,
            n: 0,
        }
    }

    fn next(&mut self, bits: u8) -> u32 {
        while self.n < bits {
            let digit = self
                .chars
                .next()
                .and_then(|c| BASE64.iter().position(|b| b == c))
                .unwrap_or(0);
            self.acc = (self.acc << 6 | digit as u32) & 0xff_ffff;
            self.n += 6;
        }
        self.n -= bits;
        (self.acc >> self.n) & ((1 << bits) - 1)
    }
}

/// The level of `v` among `2^bits` levels over `-range..range` (clamped at the ends).
#[doc(hidden)]
pub fn quantize(v: f64, bits: u8, range: f64) -> u32 {
    let levels = 1u32 << bits;
    let step = 2.0 * range / levels as f64;
    (v / step + (levels / 2) as f64)
        .floor()
        .clamp(0.0, (levels - 1) as f64) as u32
}

/// The value of a level made by [`quantize`]: the middle of its interval.
#[doc(hidden)]
pub fn dequantize(q: u32, bits: u8, range: f64) -> f64 {
    let levels = 1u32 << bits;
    let step = 2.0 * range / levels as f64;
    (q as f64 - (levels / 2) as f64 + 0.5) * step
}

/// The coefficients a hash stores the image with (all of the frequencies it can store).
#[doc(hidden)]
pub fn coefficients(rgb: &[u8], width: u32, height: u32) -> Coefficients {
    transform(&target(rgb, width, height))
}

/// The image a hash approximates: the card shrunk to [`WIDTH`]x[`HEIGHT`] and blurred in
/// linear light, in Oklab, row by row.
#[doc(hidden)]
pub fn target(rgb: &[u8], width: u32, height: u32) -> Vec<[f64; 3]> {
    let (width, height) = (width as usize, height as usize);
    assert!(width > 0 && height > 0, "the image is empty");
    assert_eq!(
        rgb.len(),
        width * height * 3,
        "not an RGB image of that size"
    );
    let b = bases();

    // shrunk in linear light: each pixel is the average of the area it covers
    let cols = area_weights(width, WIDTH);
    let rows = area_weights(height, HEIGHT);
    let mut narrow = vec![[0.0; 3]; height * WIDTH];
    for y in 0..height {
        let line = &rgb[y * width * 3..][..width * 3];
        for (x, weights) in cols.iter().enumerate() {
            let px = &mut narrow[y * WIDTH + x];
            for &(sx, w) in weights {
                for c in 0..3 {
                    px[c] += w * b.to_linear[line[sx * 3 + c] as usize];
                }
            }
        }
    }
    let mut small = vec![[0.0; 3]; HEIGHT * WIDTH];
    for (y, weights) in rows.iter().enumerate() {
        for &(sy, w) in weights {
            for x in 0..WIDTH {
                for c in 0..3 {
                    small[y * WIDTH + x][c] += w * narrow[sy * WIDTH + x][c];
                }
            }
        }
    }

    // blurred, still in linear light (down, then across)
    let mut down = vec![[0.0; 3]; HEIGHT * WIDTH];
    for y in 0..HEIGHT {
        for sy in 0..HEIGHT {
            let w = b.blur_y[y * HEIGHT + sy];
            for x in 0..WIDTH {
                for c in 0..3 {
                    down[y * WIDTH + x][c] += w * small[sy * WIDTH + x][c];
                }
            }
        }
    }
    let mut blurred = vec![[0.0; 3]; HEIGHT * WIDTH];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            for sx in 0..WIDTH {
                let w = b.blur_x[x * WIDTH + sx];
                for c in 0..3 {
                    blurred[y * WIDTH + x][c] += w * down[y * WIDTH + sx][c];
                }
            }
        }
    }
    blurred.into_iter().map(linear_to_oklab).collect()
}

/// The DCT coefficients of a [`WIDTH`]x[`HEIGHT`] Oklab image.
#[doc(hidden)]
pub fn transform(img: &[[f64; 3]]) -> Coefficients {
    let b = bases();
    // across, then down
    let mut across = vec![[[0.0; 3]; KX]; HEIGHT];
    for (row, pixels) in across.iter_mut().zip(img.chunks(WIDTH)) {
        for (x, px) in pixels.iter().enumerate() {
            for (i, freq) in row.iter_mut().enumerate() {
                let w = b.dct_x[x * KX + i];
                for c in 0..3 {
                    freq[c] += w * px[c];
                }
            }
        }
    }
    let mut coefs = [[[0.0; 3]; KX]; KY];
    for (y, row) in across.iter().enumerate() {
        for (j, freqs) in coefs.iter_mut().enumerate() {
            let w = b.dct_y[y * KY + j];
            for (freq, value) in freqs.iter_mut().zip(row) {
                for c in 0..3 {
                    freq[c] += w * value[c];
                }
            }
        }
    }
    coefs
}

/// The Oklab image of coefficients, at any size (sampled at the pixel centers), row by row.
#[doc(hidden)]
pub fn oklab_image(coefs: &Coefficients, width: u32, height: u32) -> Vec<[f64; 3]> {
    let (width, height) = (width as usize, height as usize);
    let dct_x = dct_basis(WIDTH, KX, width);
    let dct_y = dct_basis(HEIGHT, KY, height);
    // down, then across
    let mut rows = vec![[[0.0; 3]; KX]; height];
    for (y, row) in rows.iter_mut().enumerate() {
        for (j, freq) in coefs.iter().enumerate() {
            let w = dct_y[y * KY + j];
            for i in 0..KX {
                for c in 0..3 {
                    row[i][c] += w * freq[i][c];
                }
            }
        }
    }
    let mut img = Vec::with_capacity(width * height);
    for row in &rows {
        for x in 0..width {
            let mut px = [0.0; 3];
            for i in 0..KX {
                let w = dct_x[x * KX + i];
                for c in 0..3 {
                    px[c] += w * row[i][c];
                }
            }
            img.push(px);
        }
    }
    img
}

/// Decoded coefficients, as in [`decode`].
#[doc(hidden)]
pub fn render(coefs: &Coefficients, width: u32, height: u32) -> Vec<u8> {
    let to_srgb = &bases().to_srgb;
    let level = |v: f64| to_srgb[(v.clamp(0.0, 1.0) * SRGB_STEPS as f64).round() as usize];
    oklab_image(coefs, width, height)
        .into_iter()
        .flat_map(|lab| {
            let [r, g, b] = oklab_to_linear(lab);
            [level(r), level(g), level(b), u8::MAX]
        })
        .collect()
}

/// An 8-bit sRGB color in Oklab.
#[doc(hidden)]
pub fn srgb_to_oklab(rgb: [u8; 3]) -> [f64; 3] {
    linear_to_oklab(rgb.map(|v| bases().to_linear[v as usize]))
}

fn linear_to_oklab([r, g, b]: [f64; 3]) -> [f64; 3] {
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b)
        .max(0.0)
        .cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b)
        .max(0.0)
        .cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b)
        .max(0.0)
        .cbrt();
    [
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    ]
}

fn oklab_to_linear([l, a, b]: [f64; 3]) -> [f64; 3] {
    let l_ = l + 0.3963377774 * a + 0.2158037573 * b;
    let m_ = l - 0.1055613458 * a - 0.0638541728 * b;
    let s_ = l - 0.0894841775 * a - 1.2914855480 * b;
    let (l, m, s) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    [
        4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
        -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
        -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s,
    ]
}

/// Linear light values between two entries of the sRGB table.
const SRGB_STEPS: usize = 4096;

struct Bases {
    /// 8-bit sRGB to linear light
    to_linear: [f64; 256],
    /// linear light (in [`SRGB_STEPS`] steps) to 8-bit sRGB
    to_srgb: Vec<u8>,
    /// gaussian blur matrices, by output pixel then input pixel
    blur_x: Vec<f64>,
    blur_y: Vec<f64>,
    /// DCT bases at the pixels of the image, by pixel then frequency
    dct_x: Vec<f64>,
    dct_y: Vec<f64>,
}

fn bases() -> &'static Bases {
    static BASES: OnceLock<Bases> = OnceLock::new();
    BASES.get_or_init(|| Bases {
        to_linear: std::array::from_fn(|v| {
            let v = v as f64 / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        }),
        to_srgb: (0..=SRGB_STEPS)
            .map(|v| {
                let v = v as f64 / SRGB_STEPS as f64;
                let s = if v <= 0.0031308 {
                    v * 12.92
                } else {
                    1.055 * v.powf(1.0 / 2.4) - 0.055
                };
                (s * 255.0).round() as u8
            })
            .collect(),
        blur_x: blur_matrix(WIDTH),
        blur_y: blur_matrix(HEIGHT),
        dct_x: dct_basis(WIDTH, KX, WIDTH),
        dct_y: dct_basis(HEIGHT, KY, HEIGHT),
    })
}

/// Orthonormal DCT-II basis of `n` samples with `k` frequencies, evaluated at the centers of
/// `out` pixels spread over the same span (`out * k` values, by pixel then frequency).
fn dct_basis(n: usize, k: usize, out: usize) -> Vec<f64> {
    let mut m = Vec::with_capacity(out * k);
    for x in 0..out {
        let pos = (x as f64 + 0.5) * n as f64 / out as f64;
        for f in 0..k {
            let norm = (2.0 / n as f64).sqrt() * if f == 0 { 0.5f64.sqrt() } else { 1.0 };
            m.push((std::f64::consts::PI * pos * f as f64 / n as f64).cos() * norm);
        }
    }
    m
}

/// Gaussian blur of `n` samples ([`BLUR`]), with mirrored edges: a gain on each frequency
/// of the DCT (`n * n` values, by output then input sample).
fn blur_matrix(n: usize) -> Vec<f64> {
    let dct = dct_basis(n, n, n);
    let gain = (0..n)
        .map(|f| (-0.5 * (std::f64::consts::PI * BLUR * f as f64 / n as f64).powi(2)).exp())
        .collect::<Vec<_>>();
    let mut m = vec![0.0; n * n];
    for y in 0..n {
        for x in 0..n {
            m[y * n + x] = (0..n)
                .map(|f| dct[y * n + f] * gain[f] * dct[x * n + f])
                .sum();
        }
    }
    m
}

/// For each of `dst` pixels spread over `src` pixels: the pixels it covers, with the part of
/// it they cover (adding up to 1).
fn area_weights(src: usize, dst: usize) -> Vec<Vec<(usize, f64)>> {
    let scale = src as f64 / dst as f64;
    (0..dst)
        .map(|d| {
            let (start, end) = (d as f64 * scale, (d + 1) as f64 * scale);
            (start.floor() as usize..(end.ceil() as usize).min(src))
                .filter_map(|s| {
                    let cover = end.min(s as f64 + 1.0) - start.max(s as f64);
                    (cover > 0.0).then_some((s, cover / scale))
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A card-like image: a frame, art with a gradient, a text box.
    fn card(width: u32, height: u32) -> Vec<u8> {
        let mut rgb = Vec::new();
        for y in 0..height {
            for x in 0..width {
                let (fx, fy) = (x as f64 / width as f64, y as f64 / height as f64);
                let px = if !(0.04..0.96).contains(&fx) || !(0.03..0.97).contains(&fy) {
                    [30, 30, 30]
                } else if fy > 0.65 {
                    [240, 240, 235]
                } else {
                    [(250.0 * fx) as u8, 80, (250.0 * (1.0 - fy)) as u8]
                };
                rgb.extend(px);
            }
        }
        rgb
    }

    fn solid(width: u32, height: u32, color: [u8; 3]) -> Vec<u8> {
        color.repeat((width * height) as usize)
    }

    #[test]
    fn hashes_are_versioned_url_safe_and_of_fixed_size() {
        for rgb in [
            card(400, 559),
            solid(400, 559, [0; 3]),
            solid(400, 559, [255; 3]),
        ] {
            let hash = encode(&rgb, 400, 559);
            assert_eq!(hash.len(), HASH_LEN);
            assert!(hash.starts_with(VERSION));
            assert!(hash.bytes().all(|b| BASE64.contains(&b)), "{hash}");
        }
    }

    #[test]
    fn the_tables_fill_the_hash() {
        let bits: usize = TABLES.stored().map(|v| v.bits as usize).sum();
        assert!(bits <= BUDGET && bits > BUDGET - 8, "{bits} bits");
        assert!(TABLES.average.iter().all(|v| v.bits > 0));
        assert!(TABLES.components.iter().all(|c| c.value.bits > 0));
        assert_eq!(
            TABLES.basis.len(),
            TABLES.components.len() * TABLES.inputs.len()
        );
        assert!(
            TABLES
                .inputs
                .iter()
                .all(|&i| (3..COEFFICIENTS).contains(&(i as usize)))
        );
    }

    /// A real card (hBP01-104 C), as RGB.
    fn real_card() -> (Vec<u8>, u32, u32) {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/card.webp");
        let img = image::open(path).unwrap().into_rgb8();
        let (w, h) = img.dimensions();
        (img.into_raw(), w, h)
    }

    #[test]
    fn decoding_gives_the_blurred_card_back() {
        let (rgb, w, h) = real_card();
        let decoded = decode(&encode(&rgb, w, h), WIDTH as u32, HEIGHT as u32).unwrap();
        assert_eq!(decoded.len(), WIDTH * HEIGHT * 4);
        assert!(decoded.chunks(4).all(|px| px[3] == u8::MAX));
        let target = target(&rgb, w, h);
        let distances = decoded
            .chunks(4)
            .zip(&target)
            .map(|(px, t)| {
                let got = srgb_to_oklab([px[0], px[1], px[2]]);
                (0..3).map(|c| (got[c] - t[c]).powi(2)).sum::<f64>().sqrt()
            })
            .collect::<Vec<_>>();
        let mean = distances.iter().sum::<f64>() / distances.len() as f64;
        let worst = distances.iter().cloned().fold(0.0, f64::max);
        assert!(mean < 0.03 && worst < 0.12, "mean {mean}, worst {worst}");
    }

    #[test]
    fn the_average_color_is_kept() {
        // the average color is never clipped, even for colors no card has
        for color in [
            [200, 10, 10],
            [20, 90, 200],
            [255, 230, 0],
            [240, 240, 240],
            [15, 15, 20],
        ] {
            let coefs = TABLES
                .decode(&encode(&solid(80, 112, color), 80, 112))
                .unwrap();
            let average = coefs[0][0].map(|v| v / ((WIDTH * HEIGHT) as f64).sqrt());
            let expected = srgb_to_oklab(color);
            let dist = (0..3)
                .map(|c| (average[c] - expected[c]).powi(2))
                .sum::<f64>()
                .sqrt();
            assert!(dist < 0.01, "{color:?}: {average:?}, expected {expected:?}");
        }
    }

    #[test]
    fn the_hash_does_not_depend_on_the_image_size() {
        let (rgb, w, h) = real_card();
        // each pixel twice across and down
        let double = (0..h * 2)
            .flat_map(|y| (0..w * 2).map(move |x| ((y / 2) * w + x / 2) as usize))
            .flat_map(|p| rgb[p * 3..p * 3 + 3].to_vec())
            .collect::<Vec<_>>();
        assert_eq!(encode(&rgb, w, h), encode(&double, w * 2, h * 2));
    }

    #[test]
    fn other_strings_are_not_decoded() {
        let hash = encode(&card(400, 559), 400, 559);
        assert!(decode(&hash, 20, 28).is_some());
        assert!(decode(&hash[..HASH_LEN - 1], 20, 28).is_none());
        assert!(decode(&format!("{hash}A"), 20, 28).is_none());
        assert!(decode(&format!("0{}", &hash[1..]), 20, 28).is_none());
        assert!(decode(&format!("{}=", &hash[..HASH_LEN - 1]), 20, 28).is_none());
        assert!(decode("", 20, 28).is_none());
    }

    /// The format is frozen: this changes only with a new [`VERSION`].
    #[test]
    fn the_format_does_not_change() {
        let (rgb, w, h) = real_card();
        assert_eq!(encode(&rgb, w, h), GOLDEN);
    }
    const GOLDEN: &str = "1jXWDXfeh031K2VohXhmSIk1hziWXbVkzllmmW1MmKUllFqlZ7UZiJ";

    #[test]
    fn the_area_weights_cover_each_pixel_once() {
        for (src, dst) in [(400, 40), (559, 56), (41, 40), (20, 40)] {
            let weights = area_weights(src, dst);
            for w in &weights {
                assert!((w.iter().map(|(_, w)| w).sum::<f64>() - 1.0).abs() < 1e-9);
            }
            let mut per_source = vec![0.0; src];
            for (s, w) in weights.iter().flatten() {
                per_source[*s] += w;
            }
            let scale = src as f64 / dst as f64;
            assert!(per_source.iter().all(|t| (t * scale - 1.0).abs() < 1e-9));
        }
    }
}
