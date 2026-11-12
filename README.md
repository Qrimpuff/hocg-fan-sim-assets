# hocg-fan-sim-assets

A CLI utility for collecting and processing assets for the Hololive Official Card Game (hOCG) fan simulator.

## Features

- Scrape card information from multiple sources:
  - Deck Log (official card database)
  - Hololive Official website
  - @ogbajoj's fan translation sheet
  - holoDelta database
- Download card images and convert to optimized formats:
  - Official card images in WebP or optimized PNG format
  - Support for English proxy images
- Asset management:
  - Merge data from multiple sources
  - Handle card illustrations and variants
  - Create ZIP packages of card images
- Card pricing:
  - Retrieve Yuyu-tei pricing information
  - Retrieve TCGPlayer product IDs and pricing information

## Prerequisites

- Rust toolchain (2024 edition)
- For Google Sheets API access (optional):
  - Google Sheets API key (set as `GOOGLE_SHEETS_API_KEY` environment variable)
- For ScraperAPI access (optional):
  - ScraperAPI key (set as `SCRAPERAPI_API_KEY` environment variable)

## Installation

1. Clone the repository:
   ```
   git clone https://github.com/yourusername/hocg-fan-sim-assets.git
   cd hocg-fan-sim-assets
   ```

2. Build the project:
   ```
   cargo build --release
   ```

## Usage

Basic usage:
```
cargo run --release -- [OPTIONS]
```

### Command-line Options

```
Options:
  -n, --number-filter <NUMBER_FILTER>
          The card number to retrieve e.g. hSD01-001 (default to all)
  
  -x, --expansion <EXPANSION>
          The expansion to retrieve e.g. hSD01, hBP01, hPR, hYS01 (default to all)
  
  -i, --download-images
          Download card images as WebP
  
  -f, --force-download
          Always download card images as WebP
  
  -o, --optimized-original-images
          Download the original PNG images instead of converting to WebP
  
  -z, --zip-images
          Package the image into a zip file
  
  -c, --clean
          Don't read existing file
  
  -p, --proxy-path <PROXY_PATH>
          The path to the english proxy folder
  
      --assets-path <ASSETS_PATH>
          The folder that contains the assets i.e. card info, images, proxies
          [default: assets]
  
      --yuyutei [<YUYUTEI_MODE>]
          Update the yuyu-tei.jp urls for the cards. can only be use when all cards are searched
          
          Possible values:
          - quick: Use the first urls found
          - images: Compare images to find the best match
  
      --tcgplayer [<TCGPLAYER_MODE>]
          Update the TCGplayer product IDs for the cards. can only be use when all cards are searched
          
          Possible values:
          - quick: Use the first product IDs found
          - images: Compare images to find the best match
  
      --holodelta-db-path <HOLODELTA_DB_PATH>
          [deprecated] Use holoDelta to import missing/unreleased cards data. The file that contains the card database for holoDelta
  
      --holodelta-path <HOLODELTA_PATH>
          Use holoDelta to import missing/unreleased cards data. The folder that contains the source code for holoDelta
  
      --holoburi
          Use holoBuri to import art keys (e.g. SR_2) for cards. Downloads the art from holoburi.com
  
      --hololive
          Use the official holoLive website to import missing/unreleased cards data

      --deck-log
          Use the data from Deck Log to import cards data

      --language <LANGUAGE>
          The language of the cards or Q&A to import [default: all]
          Possible values:
          - all: Process both Japanese and English where available
          - japanese: Only Japanese
          - english: Only English
  
      --ogbajoj-sheet
          Use ogbajoj's sheet to import English translations
  
      --gc
          Remove unused assets

      --rebuild-image-similarity
          Recompute stored image hashes and similarity indices from local images. This can renumber existing similarity indices

      --qna
          Generate Q&A file from official sources. Compatible with --clean and --ogbajoj-sheet. Does not update cards/assets other than hocg_qnas.json
  
  -h, --help
          Print help
  
  -V, --version
          Print version
```

### Examples

1. Download all card information and images:
   ```
   cargo run --release -- --clean --deck-log --download-images
   ```

2. Download information for a specific card set:
   ```
   cargo run --release --  --deck-log --expansion hSD01 --download-images
   ```

3. Generate optimized card images with English translations:
   ```
   cargo run --release -- --deck-log --ogbajoj-sheet --download-images --optimized-original-images --proxy-path ./en_proxies
   ```

4. Create a complete dataset with pricing information:
   ```
   cargo run --release -- --clean --deck-log --hololive --ogbajoj-sheet --download-images --yuyutei=images --tcgplayer=images --proxy-path ./en_proxies --holodelta-path ./holoDelta --gc
   ```

5. Package images for specific card sets:
   ```
   cargo run --release -- --deck-log -ziofx hsd05
   cargo run --release -- --deck-log -ziofx hsd06
   cargo run --release -- --deck-log -ziofx hsd07
   ```
   This downloads images (-i), converts them to WebP, forces download (-f), packages them into ZIP files (-z), 
   and optimizes the original images (-o) for the hSD05, hSD06, and hSD07 expansions (-x).

6. Automated asset generation (from GitHub Actions):
   ```
   cargo run --release -- --deck-log --hololive --ogbajoj-sheet --download-images --yuyutei --tcgplayer --gc
   ```
   This example demonstrates daily automated asset generation (runs at 15:00 UTC/00:00 JST). It preserves existing 
   data from gh-pages branch before updating with fresh card information from all sources, then deploys to GitHub Pages.
   
7) Generate JP Q&As plus English translations from @ogbajoj's sheet:
   ```
   cargo run --release -- --qna --ogbajoj-sheet
   ```

### Reviewing illustration groups (ground truth for the image hash)

With `assets/hocg_cards.json` and the referenced images available locally, run:

```powershell
$env:PIXEL_OUT = "pixel-candidates.tsv"
$env:PIXEL_HTML_OUT = "pixel-candidates.html"
cargo test --release -p hocg-fan-sim-assets-model --test mod test_pixel_same_card_pairs -- --ignored --nocapture
```

Either output variable can be omitted. For every card number, every pair of its
Japanese and English images (excluding generated proxies) is aligned and compared
pixel by pixel. Each pair gets a pixel verdict:

- `same`: only tiny differences such as rarity stamps, foil or scan noise.
- `different`: clearly different images.
- `dubious`: a local difference that could be a changed emblem or detail, or text.

Japanese/English pairs use looser thresholds because translated text differs, so the
pixel check cannot rule out small art changes between languages. `same` pairs are
joined into suggested illustration groups for each card. A card **needs review** if it
has a dubious pair, a `different` pair inside a suggested group, or a pair where the
current `is_similar` disagrees with the suggestion. The other cards are **confident**.

Open the HTML report locally (the images are read from the local assets directory).
It shows one card at a time with its images grouped by illustration:

- Move an image to another group with the selector under it, then **Approve groups**.
- **Approve all cards on this page** records the groups shown for every visible card.
- **Bulk-approve confident cards** accepts the pixel suggestion for every confident card
  without viewing it. These are recorded as `bulk` approvals.
- Keys: `j`/`k` next/previous card, `a` approve and move on, `r` reset, `n`/`p` pages.

By default, the report lists unreviewed cards that need review. Decisions are saved in
the current browser, but browser storage can be cleared; use **Export reviews (JSON)**
to save a backup. Import merges reviews and refuses a file whose grouping conflicts
with an existing review. Reviews record each image's SHA-256, and a review becomes
outdated when the images of the card change.

To recreate the HTML from a previously generated TSV without rerunning the image
comparisons, use:

```powershell
$env:PIXEL_IN = "pixel-candidates.tsv"
$env:PIXEL_HTML_OUT = "pixel-candidates.html"
cargo test --release -p hocg-fan-sim-assets-model --test mod test_pixel_html_from_tsv -- --ignored --nocapture
```

The manually reviewed groups are committed in `model/tests/card-image-reviews.json`.
When you review more cards, replace that file with the export. Check the current hash
against it with (requires the assets directory):

```powershell
$env:PIXEL_REVIEW_IN = "card-image-reviews.json"   # optional: another review file
$env:PIXEL_REVIEW_APPROVAL = "manual"   # optional: manual, bulk or all (default)
cargo test --release -p hocg-fan-sim-assets-model --test mod test_reviewed_pairs_match_hash -- --ignored --nocapture
```

Every pair of images of a reviewed card is checked: images in the same group must be
`is_similar`, all others must not. The test lists false merges and false splits and
fails if there are any, or if an image is missing or its SHA-256 has changed.
Confident cards agree with the current hash by definition, so bulk approvals only
protect against regressions; the individually reviewed cards are where the hash
can be improved.

Image hashes are version 3 (`v3|H32=..|CYM=..|G=..|L=..`). `is_similar` compares
official images (with a `manage_id`) using a coarse color grid of the card, with
thresholds that depend only on whether both images have the same language and rarity.
Unreleased images (samples, sheet crops, scans) and older v2 hashes are compared with
the tolerant perceptual hash distance, which is also used to match images from other
sources. The only per-card exception is `SAME_ACROSS_LANGUAGES` in `img_hash.rs`, for
prints whose layout differs between languages. Older hashes do not have the grid, so
rebuild them as described below.

The tests recompute hashes from the images and never modify the asset database.
Stored hashes and similarity indices are not updated when the hash changes. To rebuild
them, back up the asset database and run:

```
cargo run --release -- --rebuild-image-similarity
```

Run it right after updating to a new hash version: until then, new hashes are compared
with old ones using only the perceptual hash distance. The hash of an official
illustration comes from its official Japanese image when there is one, otherwise from
its official English image; the rebuild fails without changing anything if one of
these images is missing or unreadable. Unreleased illustrations keep their hash, which
comes from where their image was found (e.g. a sheet thumbnail). All illustrations are
then regrouped, so existing similarity indices can change. The rebuild cannot recover
illustrations that have already been merged into one entry.

## Directory Structure

- `assets/` - Output directory for all generated assets
  - `hocg_cards.json` - Complete card database
  - `hocg_qnas.json` - Generated questions & answers database
  - `img/` - Downloaded card images (Japanese)
  - `img_en/` - Proxy card images (English)

## Data Sources

- **Deck Log** - The primary source for card information
- **Hololive Official Website** - Additional card info for unreleased cards
- **@ogbajoj's Sheet** - English translations and additional card details
- **holoDelta Database** - Additional card image variants
- **Yuyu-tei** - Japanese card pricing and marketplace information
- **TCGPlayer** - English card pricing and marketplace information

## Development

The project is structured as a Rust workspace with two crates:
- `model` - Data models and structures
- `cli` - Command-line interface and functionality

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

## Acknowledgements

- Hololive Production for the card game
- @ogbajoj for maintaining the English card translation sheet
- holoDelta project for additional card data
