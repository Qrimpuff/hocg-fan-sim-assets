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

## Prerequisites

- Rust toolchain
- For Google Sheets API access (optional):
  - Google Sheets API key (set as `GOOGLE_SHEETS_API_KEY` environment variable)

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
  
      --skip-update
          Don't update the cards info
  
      --yuyutei-urls
          Update the yuyu-tei.jp urls for the cards. can only be use when all cards are searched
  
      --holodelta-db-path <HOLODELTA_DB_PATH>
          Use holoDelta to import missing/unreleased cards data. The file that contains the card database for holoDelta
  
      --official-hololive
          Use the official holoLive website to import missing/unreleased cards data
  
      --ogbajoj-sheet
          Use ogbajoj's sheet to import English translations
  
  -h, --help
          Print help
  
  -V, --version
          Print version
```

### Examples

1. Download all card information and images:
   ```
   cargo run --release -- --clean --download-images
   ```

2. Download information for a specific card set:
   ```
   cargo run --release -- --expansion hSD01 --download-images
   ```

3. Generate optimized card images with English translations:
   ```
   cargo run --release -- --download-images --optimized-original-images --ogbajoj-sheet --proxy-path ./en_proxies
   ```

4. Create a complete dataset with pricing information and packaging:
   ```
   cargo run --release -- --clean --download-images --yuyutei-urls --proxy-path ./en_proxies --holodelta-db-path ./cardData.db --ogbajoj-sheet --official-hololive --zip-images
   ```

5. Package images for specific card sets (from package-images.cmd):
   ```
   cargo run --release -- -ziofx hsd05
   cargo run --release -- -ziofx hsd06
   cargo run --release -- -ziofx hsd07
   ```
   This downloads images (-i), converts them to WebP, forces download (-f), packages them into ZIP files (-z), 
   and optimizes the original images (-o) for the hSD05, hSD06, and hSD07 expansions (-x).

6. Automated asset generation (from GitHub Actions):
   ```
   cargo run --release -- --download-images --yuyutei-urls --ogbajoj-sheet --official-hololive
   ```
   This example demonstrates daily automated asset generation (runs at 00:00 JST/15:00 UTC). It preserves existing 
   data from gh-pages branch before updating with fresh card information from all sources, then deploys to GitHub Pages.

## Directory Structure

- `assets/` - Output directory for all generated assets
  - `hocg_cards.json` - Complete card database
  - `img/` - Downloaded card images (Japanese)
  - `img_en/` - Proxy card images (English)

## Data Sources

- **Deck Log** - The primary source for card information
- **Hololive Official Website** - Additional card info for unreleased cards
- **@ogbajoj's Sheet** - English translations and additional card details
- **holoDelta Database** - Additional card image variants

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
