use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    error::Error,
    fs,
    ops::Not,
    path::Path,
    sync::{Arc, LazyLock},
    thread::sleep,
    time::Duration,
};

use crate::img_hash::{
    DIST_TOLERANCE_DIFF_RARITY, DIST_TOLERANCE_SAME_RARITY, Grid, ImageGrids, art_distance,
    dist_hash, to_image_hash,
};
use hocg_fan_sim_assets_model::{
    Art, BloomLevel, Card, CardIllustration, CardRarity, CardType, CardsDatabase, Color, Extra,
    Keyword, KeywordEffect, Language, Localized, OshiSkill, OshiSkillKind, ProductsDatabase,
    QnaDatabase, SheetCell, SheetId, SupportType, Tag,
};
use image::{DynamicImage, RgbImage};
use itertools::Itertools;
use parking_lot::RwLock;
use rayon::iter::{IntoParallelIterator, IntoParallelRefIterator, ParallelIterator};
use reqwest::StatusCode;
use reqwest::header::REFERER;
use scraper::{Html, Node, Selector};
use serde::Deserialize;
use webp::{Encoder, WebPMemory};

use crate::{
    DEBUG,
    data::{update_arts, update_extra, update_keywords, update_oshi_skills, update_tags},
    google_docs_http_client, google_sheets_api_http_client,
    images::{PROXIES_FOLDER, UNRELEASED_FOLDER, WEBP_QUALITY},
    products::{
        UnofficialName, comparable_name, drop_product_sources, parse_source_lines,
        promo_sources_of, row_product, split_language_tag, tab_category, tab_product_codes,
        update_short_names, update_unofficial_names,
    },
    utils::{TrimOnce, sanitize_filename},
};

const SPREADSHEET_ID: &str = "1IdaueY-Jw8JXjYLOhA9hUd2w0VRBao9Z1URJwmCWJ64";
const GOOGLE_FETCH_MAX_ATTEMPTS: u32 = 5;

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Spreadsheet {
    pub _properties: SpreadsheetProperties,
    pub sheets: Vec<Sheet>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SpreadsheetProperties {
    pub _title: String,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Sheet {
    pub properties: SheetProperties,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SheetProperties {
    pub sheet_id: u64,
    pub title: String,
}

#[derive(Debug)]
pub struct SheetCard {
    pub sheet_id: u64,
    pub row_idx: usize,
    pub col_idx: usize,
    pub set_code: String,
    pub card_name_jp_en: String,
    pub language: Language,
    pub images_src: String,
    pub full_size_img_url: String,
    pub hash_img_url: String,
    pub card_type: String,
    pub rarity: String,
    pub color: String,
    pub life_hp: String,
    pub tags: String,
    pub baton_pass: String,
    pub text: String,
    /// The "Source" / "Source(s)" column, `None` when the tab doesn't have one.
    pub source: Option<String>,
    /// The "Pack" column (one product per line), `None` when the tab doesn't have one.
    pub pack: Option<String>,
}

impl SheetCard {
    fn update_card(&mut self, card: &mut Card) {
        let card_number = card.card_number.clone();
        let jp_released = card
            .illustrations
            .iter()
            .any(|i| i.manage_id.japanese.is_some());
        let en_released = card
            .illustrations
            .iter()
            .any(|i| i.manage_id.english.is_some());
        let released = jp_released || en_released;

        // don't overwrite if it already exists
        // Card number
        if card.card_number.is_empty() || !released {
            card.card_number = self.set_code.clone();
        } else {
            // warn if the card number is different
            if card.card_number != self.set_code {
                eprintln!(
                    "Warning: {card_number} number mismatch: {} should be {}",
                    self.set_code, card.card_number
                );
            }
        }

        // Card name
        let name = self.name();
        if card.name.japanese.is_none() || !jp_released {
            // fix Elizabeth Rose Bloodflame - エリザベス・ローズ・ブラッドフレイム
            card.name.japanese = name.japanese.as_ref().map(|n| n.replace("\n", "・"));
        } else {
            // warn if the name is different (ignore names that are split)
            if card.name.japanese != name.japanese
                && name
                    .japanese
                    .as_ref()
                    .is_some_and(|n| !n.contains("\n") && !n.contains("&"))
            {
                eprintln!(
                    "Warning: {card_number} name mismatch: {:?} should be {:?}",
                    name.japanese, card.name.japanese
                );
            }
        }
        if card.name.english.is_none() || !en_released {
            card.name.english = name.english.clone();
        }

        // Card type
        if card.card_type == Default::default() || !released {
            card.card_type = self.card_type();
        } else {
            // warn if the card type is different
            let card_type = self.card_type();
            if card.card_type != card_type {
                eprintln!(
                    "Warning: {card_number} type mismatch: {:?} should be {:?}",
                    card_type, card.card_type
                );
            }
        }

        // Color
        if card.colors.is_empty() || !released {
            card.colors = self.colors();
        } else {
            // warn if the colors are different
            let mut colors_1 = self.colors().clone();
            colors_1.sort();
            let mut colors_2 = card.colors.clone();
            colors_2.sort();
            if colors_1 != colors_2 {
                eprintln!(
                    "Warning: {card_number} colors mismatch: {colors_1:?} should be {colors_2:?}"
                );
            }
        }

        // Life / HP
        if card.card_type == CardType::OshiHoloMember {
            if card.life == 0 || !released {
                card.life = self.life_hp.parse().unwrap_or_default();
            } else {
                // warn if the life is different
                let life = self.life_hp.parse::<u32>().unwrap_or_default();
                if card.life != life {
                    eprintln!(
                        "Warning: {card_number} life mismatch: {} should be {}",
                        life, card.life
                    );
                }
            }
        } else if card.card_type == CardType::HoloMember {
            if card.hp == 0 || !released {
                card.hp = self.life_hp.parse().unwrap_or_default();
            } else {
                // warn if the hp is different
                let hp = self.life_hp.parse::<u32>().unwrap_or_default();
                if card.hp != hp {
                    eprintln!(
                        "Warning: {card_number} hp mismatch: {} should be {}",
                        hp, card.hp
                    );
                }
            }
        }

        // Baton pass
        if card.card_type == CardType::HoloMember {
            fix_baton_pass(card, &mut self.baton_pass);
            if card.baton_pass.is_empty() || !released {
                card.baton_pass = std::iter::repeat_n(
                    Color::Colorless,
                    self.baton_pass.parse().unwrap_or_default(),
                )
                .collect();
            } else {
                // warn if the baton pass is different
                let baton_pass = self.baton_pass.parse::<usize>().unwrap_or_default();
                if card.baton_pass.len() != baton_pass {
                    eprintln!(
                        "Warning: {card_number} baton pass mismatch: {} should be {}",
                        baton_pass,
                        card.baton_pass.len()
                    );
                }
            }
        }

        // Bloom level, Buzz, Limited
        if card.bloom_level == Default::default() || !released {
            card.bloom_level = self.bloom_level();
            card.buzz = self.buzz();
            card.limited = self.limited();
        } else {
            // warn if the bloom level is different
            let bloom_level = self.bloom_level();
            if card.bloom_level != bloom_level {
                eprintln!(
                    "Warning: {card_number} bloom level mismatch: {:?} should be {:?}",
                    bloom_level, card.bloom_level
                );
            }
            // warn if the buzz is different
            let buzz = self.buzz();
            if card.buzz != buzz {
                eprintln!(
                    "Warning: {card_number} buzz mismatch: {} should be {}",
                    buzz, card.buzz
                );
            }
            // warn if the limited is different
            let limited = self.limited();
            if card.limited != limited {
                eprintln!(
                    "Warning: {card_number} limited mismatch: {} should be {}",
                    limited, card.limited
                );
            }
        }

        // Ability text and other texts (oshi skills, keywords, arts, extra)
        self.update_card_text(card, released, en_released);
        // there is no japanese text in the sheet
        // update existing tags (tags consistency check)
        update_tags(
            card,
            self.tags(),
            Language::English,
            !released,
            !en_released,
        );
        // there is no baton pass in the sheet
        // there is no max amount in the sheet, add default max amount
        if card.max_amount.japanese.unwrap_or_default() == 0 || !jp_released {
            card.max_amount.japanese = Some(match card.card_type {
                CardType::OshiHoloMember => 1,
                CardType::Cheer => 20,
                _ => 4,
            });

            if card
                .extra
                .as_ref()
                .filter(|e| {
                    e.english.as_deref()
                        == Some("You may include any number of this holomem in the deck")
                })
                .is_some()
            {
                card.max_amount.japanese = Some(50);
            }
        }
    }

    fn update_card_text(&self, card: &mut Card, released: bool, en_released: bool) {
        let text = self.text();
        let mut text_lines = text.lines().map(|l| l.trim()).collect_vec();

        fn extract_sections<'a>(lines: &mut Vec<&'a str>, starts: &[&str]) -> Vec<Vec<&'a str>> {
            let mut sections = Vec::new();
            let mut current_section = Vec::new();
            let mut in_section = false;

            lines.retain(|line| {
                if !in_section
                    && starts
                        .iter()
                        .any(|start| line.to_lowercase().starts_with(&start.to_lowercase()))
                {
                    in_section = true;
                }
                if in_section {
                    if line.is_empty() {
                        in_section = false;
                        if !current_section.is_empty() {
                            sections.push(current_section.clone());
                            current_section.clear();
                        }
                    } else {
                        current_section.push(*line);
                    }
                    false
                } else {
                    true
                }
            });

            if !current_section.is_empty() {
                sections.push(current_section);
            }

            sections
        }

        // Oshi skills
        let oshi_skills_lines = extract_sections(
            &mut text_lines,
            &["Oshi Skill", "SP Oshi Skill", "Oshi Stage Skill"],
        );
        let oshi_skills = oshi_skills_lines
            .into_iter()
            .filter_map(|lines| self.to_oshi_skill(card, lines.into()))
            .collect_vec();
        // dbg!(&oshi_skills);
        update_oshi_skills(
            card,
            oshi_skills,
            Language::English,
            !released,
            !en_released,
        );

        // Keywords
        let keywords_lines =
            extract_sections(&mut text_lines, &["Collab Effect", "Bloom Effect", "Gift"]);
        let keywords = keywords_lines
            .into_iter()
            .filter_map(|lines| self.to_keyword(card, lines.into()))
            .collect_vec();
        update_keywords(card, keywords, Language::English, !released, !en_released);

        // Arts
        let arts_lines = extract_sections(&mut text_lines, &["Arts"]);
        let arts = arts_lines
            .into_iter()
            .filter_map(|lines| self.to_art(card, lines.into()))
            .collect_vec();
        update_arts(card, arts, Language::English, !released, !en_released);

        // Extra
        let extra_lines = extract_sections(&mut text_lines, &["Extra"]);
        let mut extra = extra_lines
            .into_iter()
            .filter_map(|lines| self.to_extra(card, lines.into()))
            .next();
        fix_extra(card, &mut extra);
        update_extra(card, extra, Language::English, !released, !en_released);

        // Ability text
        if !en_released {
            card.ability_text.english = text_lines
                .is_empty()
                .not()
                .then(|| text_lines.join("\n").trim().to_string());
        }
    }

    fn to_oshi_skill(&self, card: &mut Card, mut lines: VecDeque<&str>) -> Option<OshiSkill> {
        let first = lines.pop_front().unwrap_or_default();
        let Some((mut holo_power, name)) = first.split_once(':') else {
            eprintln!("Warning: Oshi skill not found for {}", card.card_number);
            return None;
        };

        let mut kind = Default::default();
        if holo_power.starts_with("Oshi Skill") {
            kind = OshiSkillKind::Normal;
            holo_power = holo_power.trim_start_once("Oshi Skill").trim();
        } else if holo_power.starts_with("SP Oshi Skill") {
            kind = OshiSkillKind::Special;
            holo_power = holo_power.trim_start_once("SP Oshi Skill").trim();
        } else if holo_power.starts_with("Oshi Stage Skill") {
            kind = OshiSkillKind::Stage;
            holo_power = holo_power.trim_start_once("Oshi Stage Skill").trim();
        }
        holo_power = holo_power.trim_start_once("holo Power").trim();
        holo_power = holo_power.trim_start_once("-");
        let holo_power = holo_power
            .is_empty()
            .not()
            .then(|| holo_power.to_string().try_into().ok())
            .flatten();

        let name = name.trim();

        Some(OshiSkill {
            kind,
            holo_power,
            name: Localized::en(name.into()),
            ability_text: Localized::en(lines.into_iter().join("\n").trim().to_string()),
        })
    }

    fn to_keyword(&self, card: &mut Card, mut lines: VecDeque<&str>) -> Option<Keyword> {
        let first = lines.pop_front().unwrap_or_default();
        let Some((effect, name)) = first.split_once(':') else {
            eprintln!("Warning: Keyword not found for {}", card.card_number);
            return None;
        };
        let effect = match effect.to_lowercase().trim() {
            "collab effect" => KeywordEffect::Collab,
            "bloom effect" => KeywordEffect::Bloom,
            "gift" | "gift effect" => KeywordEffect::Gift,
            _ => {
                eprintln!("Unknown keyword effect: {effect}");
                return None;
            }
        };

        let name = name.trim();

        Some(Keyword {
            effect,
            name: Localized::en(name.into()),
            ability_text: Localized::en(lines.into_iter().join("\n").trim().to_string()),
        })
    }

    fn to_art(&self, card: &mut Card, mut lines: VecDeque<&str>) -> Option<Art> {
        let first = lines.pop_front().unwrap_or_default();
        let Some((_arts, name)) = first.split_once(':') else {
            eprintln!("Warning: Art not found for {}", card.card_number);
            return None;
        };
        let name = name.trim();

        let second = lines.pop_front().unwrap_or_default();
        let Some((_cost, cheers)) = second.split_once(':') else {
            eprintln!("Warning: Art cost not found for {}", card.card_number);
            return None;
        };

        let cheers = cheers
            .split(',')
            .map(|s| s.trim())
            .flat_map(|s| match s.split_once(' ') {
                Some((amount, color)) => {
                    let amount = amount.parse().unwrap_or_default();
                    let color = match color.to_lowercase().as_str() {
                        "white" => Color::White,
                        "green" => Color::Green,
                        "red" => Color::Red,
                        "blue" => Color::Blue,
                        "purple" => Color::Purple,
                        "yellow" => Color::Yellow,
                        _ => Color::Colorless,
                    };
                    std::iter::repeat_n(color, amount).collect_vec()
                }
                _ => vec![],
            })
            .collect_vec();

        let third = lines.pop_front().unwrap_or_default();
        let Some((_power, power)) = third.split_once(':') else {
            eprintln!("Warning: Art power not found for {}", card.card_number);
            return None;
        };

        let (power, advantage) = if let Some((power, advantage)) = power.split_once(',') {
            let power = power.trim().to_string().try_into().unwrap_or_default();

            let advantage = advantage.split_once("vs").map_or_else(
                || None,
                |(advantage, color)| {
                    let color = match color.trim().to_lowercase().as_str() {
                        "white" => Color::White,
                        "green" => Color::Green,
                        "red" => Color::Red,
                        "blue" => Color::Blue,
                        "purple" => Color::Purple,
                        "yellow" => Color::Yellow,
                        _ => Color::Colorless,
                    };
                    let advantage = advantage
                        .trim()
                        .trim_start_once("+")
                        .parse()
                        .unwrap_or_default();
                    Some((color, advantage))
                },
            );

            (power, advantage)
        } else {
            let power = power.trim().to_string().try_into().unwrap_or_default();
            (power, None)
        };

        Some(Art {
            cheers,
            name: Localized::en(name.into()),
            power,
            advantage,
            ability_text: lines
                .is_empty()
                .not()
                .then(|| Localized::en(lines.into_iter().join("\n").trim().to_string())),
        })
    }

    fn to_extra(&self, card: &mut Card, mut lines: VecDeque<&str>) -> Option<Extra> {
        let first = lines.pop_front().unwrap_or_default();
        let Some((_extra, extra)) = first.split_once(':') else {
            eprintln!("Warning: Extra not found for {}", card.card_number);
            return None;
        };

        Some(Localized::en(extra.trim().into()))
    }

    fn name(&self) -> Localized<String> {
        if let Some((jp, en)) = self.card_name_jp_en.split_once("\n(") {
            Localized::both(
                jp.trim().into(),
                en.trim_start_once("(").trim_end_once(")").trim().into(),
            )
        } else {
            let name = self.card_name_jp_en.trim();
            Localized::both(name.into(), name.into())
        }
    }

    fn card_type(&self) -> CardType {
        match self.card_type.trim().to_lowercase().as_str() {
            s if s.contains("oshi") => CardType::OshiHoloMember,
            s if s.contains("holomem") => CardType::HoloMember,
            s if s.contains("staff") => CardType::Support(SupportType::Staff),
            s if s.contains("item") => CardType::Support(SupportType::Item),
            s if s.contains("event") => CardType::Support(SupportType::Event),
            s if s.contains("tool") => CardType::Support(SupportType::Tool),
            s if s.contains("mascot") => CardType::Support(SupportType::Mascot),
            s if s.contains("fan") => CardType::Support(SupportType::Fan),
            "cheer" => CardType::Cheer,
            _ => CardType::Other,
        }
    }

    fn colors(&self) -> Vec<Color> {
        self.color
            .split('/')
            .map(|s| s.trim().to_lowercase())
            .flat_map(|s| match s.as_str() {
                "white" => Some(Color::White),
                "green" => Some(Color::Green),
                "red" => Some(Color::Red),
                "blue" => Some(Color::Blue),
                "purple" => Some(Color::Purple),
                "yellow" => Some(Color::Yellow),
                "none" => Some(Color::Colorless),
                _ => None,
            })
            .collect()
    }

    fn bloom_level(&self) -> Option<BloomLevel> {
        let card_type = self.card_type.trim().to_lowercase();
        if card_type.contains("1st") {
            Some(BloomLevel::First)
        } else if card_type.contains("2nd") {
            Some(BloomLevel::Second)
        } else if card_type.contains("debut") {
            Some(BloomLevel::Debut)
        } else if card_type.contains("spot") {
            Some(BloomLevel::Spot)
        } else {
            None
        }
    }

    fn buzz(&self) -> bool {
        self.card_type.trim().to_lowercase().contains("buzz")
    }

    fn limited(&self) -> bool {
        self.text.trim().to_lowercase().contains("limited:")
    }

    fn text(&self) -> String {
        self.text
            .lines()
            .map(|l| l.trim())
            // should remove the "LIMITED: Only one can be used per turn." line
            .filter(|l| !l.to_lowercase().starts_with("limited:"))
            // should remove the "[*translator's comment*]" line
            .filter(|l| !(l.starts_with('[') && l.ends_with(']')))
            .collect_vec()
            .join("\n")
            .trim()
            .to_string()
    }

    fn tags(&self) -> Vec<Tag> {
        self.tags
            .split('#')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| Localized::en(format!("#{s}")))
            .collect()
    }
}

// --- Fix known issues with the spreadsheet ---
fn fix_baton_pass(card: &mut Card, baton_pass: &mut String) {
    // fix missing baton pass
    // hBP01-021 - Tokino Sora
    // hBP01-044 - AZKi
    if card.card_number == "hBP01-021" || card.card_number == "hBP01-044" {
        *baton_pass = "1".to_string();
    }
}

fn fix_extra(card: &Card, extra: &mut Option<Extra>) {
    // fix missing Buzz extra text
    if card.buzz && extra.is_none() {
        *extra = Some(Localized::en(
            "If this holomem is downed, you get life-2".into(),
        ));
    }

    // fix missing Spot extra text
    if card.bloom_level == Some(BloomLevel::Spot) && extra.is_none() {
        *extra = Some(Localized::en("This holomem cannot bloom".into()));
    }
}
// --- End of fixes ---

// Retrieve the following fields from @ogbajoj's sheet:
// - Card number
// - Card name "JP (EN)"
// - Type (e.g. 1st Bloom holomem)
// - Color
// - LIFE/HP
// - Tags
// - Text
pub fn retrieve_card_info_from_ogbajoj_sheet(
    all_cards: &mut CardsDatabase,
    cache_dir: Option<&Path>,
) {
    println!("Retrieve all cards info from @ogbajoj's sheet");

    let Some(spreadsheet) = retrieve_spreadsheet(cache_dir) else {
        eprintln!("Warning: unable to retrieve spreadsheet metadata; skipping card info import");
        return;
    };

    let mut updated_count = 0;

    for sheet in spreadsheet.sheets {
        // Skip Master Sheet. Less reliable data.
        if sheet.properties.title == "Master Sheet" {
            continue;
        }

        let cards = retrieve_spreadsheet_data(&sheet, cache_dir).unwrap_or_default();

        for result in cards {
            let mut record: SheetCard = result;
            // println!("{:#?}", record);

            // skip empty records
            if record.set_code.trim().is_empty() || record.card_name_jp_en.trim().is_empty() {
                continue;
            }

            let Some(card) = all_cards.get_mut(&record.set_code) else {
                // println!("Card {} not found", record.set_code);
                continue;
            };
            record.update_card(card);
            updated_count += 1;
        }
    }

    println!("Updated {updated_count} cards");

    // the cheers without a name get the one of their set, see `fill_cheer_basics`
    let missing_english = all_cards
        .values()
        .filter(|c| c.card_type != CardType::Cheer && c.name.english.is_none())
        .count();
    println!("Missing english names: {missing_english}");
}

pub fn download_images_from_ogbajoj_sheet(
    images_jp_path: &Path,
    images_en_path: &Path,
    all_cards: &mut CardsDatabase,
    cache_dir: Option<&Path>,
) {
    println!("Downloading unreleased images from @ogbajoj's sheet...");

    let all_cards = Arc::new(RwLock::new(all_cards));

    let Some(spreadsheet) = retrieve_spreadsheet(cache_dir) else {
        eprintln!("Warning: unable to retrieve spreadsheet metadata; skipping sheet image import");
        return;
    };

    // this is used to delay Master Sheet access
    let master_sheet_lock = Arc::new(RwLock::new(()));

    // Iterate html files inside the ZIP
    spreadsheet
        .sheets
        .into_par_iter()
        .filter_map(move |sheet| {
            let name = sheet.properties.title.clone();
            let sheet_id = sheet.properties.sheet_id;

            // Skip Master Sheet. Low quality images, wrong data, and duplicates (2025-09)
            if name == "Master Sheet" {
                return None;
            }

            let cards = retrieve_spreadsheet_data(&sheet, cache_dir);

            Some((name, sheet_id, cards?))
        })
        .inspect({
            let all_cards = all_cards.clone();
            move |(_, sheet_id, cards)| {
                // remove orphan sheet_cells
                all_cards
                    .write()
                    .values_mut()
                    .flat_map(|cs| cs.illustrations.iter_mut())
                    .filter(|c| {
                        c.ogbajoj_sheet_cells
                            .iter()
                            .flatten()
                            .any(|(s, _)| s == sheet_id)
                    })
                    .for_each(|c| {
                        if let Some(ogbajoj_sheet_cells) = c.ogbajoj_sheet_cells.as_mut() {
                            ogbajoj_sheet_cells.retain(|(s, c)| {
                                let (row_idx, col_idx) =
                                    c.to_row_col().expect("should be a valid sheet cell");
                                s != sheet_id
                                    || cards.iter().any(|card| {
                                        s == sheet_id
                                            && card.row_idx == row_idx
                                            && card.col_idx == col_idx
                                    })
                            });
                        };
                        // remove empty sheet_cell
                        c.ogbajoj_sheet_cells.take_if(|v| v.is_empty());
                    });
            }
        })
        .for_each({
            let all_cards = all_cards.clone();
            let master_sheet_lock = master_sheet_lock.clone();
            move |(name, _, cards)| {
                let mut imported = 0;
                let mut skipped = 0;

                // this is used to delay Master Sheet access
                let _lock = master_sheet_lock.read();
                if name == "Master Sheet" {
                    drop(_lock);
                    let _exclusive = master_sheet_lock.write();
                }

                // Process data rows after the header row
                for card in cards {
                    let language = card.language;
                    let row_idx = card.row_idx;
                    let col_idx = card.col_idx;
                    let set_code = card.set_code;
                    let rarity = card.rarity;
                    let img_src = card.images_src;
                    let hash_img_url = card.hash_img_url;
                    let full_size_img_url = card.full_size_img_url;
                    let sheet_id = card.sheet_id;
                    let sheet_cell = SheetCell::from_row_col(row_idx, col_idx);

                    // language switch for English exclusive cards like hY01-008
                    let images_path = match language {
                        Language::Japanese => images_jp_path,
                        Language::English => images_en_path,
                    };

                    if DEBUG {
                        println!("[{name}] row: {set_code} -> {img_src}");
                    }

                    // Download and decode image
                    let Ok(hash_img) =
                        download_sheet_image(&hash_img_url, row_idx, &set_code, &name)
                    else {
                        continue;
                    };
                    let mut img_hash = to_image_hash(&hash_img.into_rgb8(), Some(language));
                    fix_image_hash(&mut img_hash);

                    // remove the old sheet_cell, if it exists
                    all_cards
                        .write()
                        .values_mut()
                        .flat_map(|cs| cs.illustrations.iter_mut())
                        .filter(|c| {
                            c.card_number != set_code
                                && c.ogbajoj_sheet_cells
                                    .iter()
                                    .flatten()
                                    .any(|(s, c)| *s == sheet_id && *c == sheet_cell)
                        })
                        .for_each(|c| {
                            if let Some(ogbajoj_sheet_cells) = c.ogbajoj_sheet_cells.as_mut() {
                                ogbajoj_sheet_cells
                                    .retain(|(s, c)| *s != sheet_id || *c != sheet_cell);
                            };
                            // remove empty sheet_cell
                            c.ogbajoj_sheet_cells.take_if(|v| v.is_empty());
                        });

                    let mut _adding = false;
                    let mut img_unreleased = Path::new(UNRELEASED_FOLDER)
                        .join(sanitize_filename(&format!("{}_{}.webp", set_code, rarity)));
                    {
                        // Find the card or create
                        let mut all_cards = all_cards.write();
                        let card = all_cards.entry(set_code.clone()).or_default();

                        // find a new image file name
                        let mut counter = 2;
                        while card.illustrations.iter().any(|i| {
                            i.img_path
                                .value(language)
                                .as_ref()
                                .map(|p| *p == img_unreleased.to_str().unwrap().replace("\\", "/"))
                                .unwrap_or(false)
                        }) {
                            img_unreleased = Path::new(UNRELEASED_FOLDER).join(sanitize_filename(
                                &format!("{}_{}_{}.webp", set_code, rarity, counter),
                            ));
                            counter += 1;
                        }

                        // find a matching illustration, otherwise create a new one
                        let mut matching_illustrations = card
                            .illustrations
                            .iter_mut()
                            .map(|i| {
                                // remove existing cell, for when the card move cells
                                let mut same_sheet_cell = false;
                                if let Some(ogbajoj_sheet_cells) = i.ogbajoj_sheet_cells.as_mut() {
                                    same_sheet_cell = ogbajoj_sheet_cells
                                        .iter()
                                        .any(|(s, c)| *s == sheet_id && *c == sheet_cell);
                                    ogbajoj_sheet_cells
                                        .retain(|(s, c)| *s != sheet_id || *c != sheet_cell);
                                };
                                // remove empty sheet_cell
                                i.ogbajoj_sheet_cells.take_if(|v| v.is_empty());
                                (i, same_sheet_cell)
                            })
                            .filter(|(i, _)| {
                                i.rarity.to_string().eq_ignore_ascii_case(&rarity)
                                    || !i.manage_id.has_value()
                            })
                            .map(|(i, s)| (dist_hash(&i.img_hash, &img_hash), i, s))
                            // more tolerance for manual cropping
                            .filter(|(dist, i, same_sheet_cell)| {
                                match (
                                    i.manage_id.has_value(),                            // released
                                    i.rarity.to_string().eq_ignore_ascii_case(&rarity), // same rarity
                                ) {
                                    (true, true) => *dist <= DIST_TOLERANCE_SAME_RARITY,
                                    (true, false) => unreachable!(),
                                    (false, true) => {
                                        // allow modifying the same unreleased cell
                                        *same_sheet_cell || *dist <= DIST_TOLERANCE_SAME_RARITY
                                    }
                                    (false, false) => *dist <= DIST_TOLERANCE_DIFF_RARITY,
                                }
                            })
                            .collect_vec();
                        matching_illustrations.sort_by_key(|(dist, _, _)| *dist);

                        let illust = if let Some((_, illust, _)) =
                            matching_illustrations.first_mut()
                        {
                            // add the sheet cell back, if not present
                            illust
                                .ogbajoj_sheet_cells
                                .get_or_insert_default()
                                .insert((sheet_id, sheet_cell));

                            // Use existing unreleased illustration, with a different image
                            // Master Sheet has duplicate entries
                            if !illust.manage_id.has_value()
                                && illust.img_hash != img_hash
                                && name != "Master Sheet"
                            {
                                _adding = false;

                                // if the rarity is different, rename the new file if needed
                                if let Some(img_path) = illust.img_path.value(language)
                                    && illust.rarity.to_string() == rarity
                                {
                                    img_unreleased = Path::new(img_path).into();
                                } else {
                                    illust.rarity = rarity.clone().into();
                                    *illust.img_path.value_mut(language) =
                                        Some(img_unreleased.to_str().unwrap().replace("\\", "/"))
                                }

                                illust
                            } else {
                                // it's an unreleased card that might have an updated rarity
                                if !illust.manage_id.has_value()
                                    && illust.rarity.to_string() != rarity
                                {
                                    illust.rarity = rarity.clone().into();
                                    // rename the old file
                                    if let Some(img_path) =
                                        illust.img_path.value_mut(language).as_mut()
                                    {
                                        let old_path = images_path.join(&img_path);
                                        *img_path =
                                            img_unreleased.to_str().unwrap().replace("\\", "/");
                                        let new_path = images_path.join(&img_path);
                                        if old_path != new_path
                                            && let Err(err) = fs::rename(&old_path, &new_path)
                                        {
                                            eprintln!(
                                                "Warning: failed to rename image {} -> {}: {err}",
                                                old_path.display(),
                                                new_path.display()
                                            );
                                        }
                                    }
                                }

                                // already exists
                                skipped += 1;
                                continue;
                            }
                        } else {
                            _adding = true;

                            // Doesn't exist, add illustration
                            card.illustrations.push(CardIllustration {
                                card_number: set_code.clone(),
                                rarity: rarity.clone().into(),
                                img_path: Localized::new(
                                    language,
                                    img_unreleased.to_str().unwrap().replace("\\", "/"),
                                ),
                                ogbajoj_sheet_cells: Some([(sheet_id, sheet_cell.clone())].into()),
                                ..Default::default()
                            });
                            card.illustrations.last_mut().unwrap()
                        };

                        // could be cleared on errors, with path
                        illust.img_hash = img_hash.clone();
                        illust.similarity_index = 0;
                    }

                    // Download and decode image
                    let mut error = false;
                    if let Ok(full_size_img) =
                        download_sheet_image(&full_size_img_url, row_idx, &set_code, &name)
                    {
                        // Resize image
                        let resized_img = full_size_img.resize_exact(
                            400,
                            559,
                            image::imageops::FilterType::Lanczos3,
                        );
                        // Create the WebP encoder for the above image
                        let encoder: Encoder = Encoder::from_image(&resized_img).unwrap();
                        // Encode the image at a specified quality 0-100
                        let webp: WebPMemory = encoder.encode(WEBP_QUALITY);
                        // Define and write the WebP-encoded file to a given path
                        let path = images_path.join(&img_unreleased);
                        if let Some(parent) = Path::new(&path).parent() {
                            fs::create_dir_all(parent).unwrap();
                        }
                        std::fs::write(&path, &*webp).unwrap();
                    } else {
                        error = true;
                    }

                    if error {
                        let mut all_cards = all_cards.write();
                        let illust = all_cards
                            .get_mut(&set_code)
                            .unwrap()
                            .illustrations
                            .iter_mut()
                            .find(|i| i.rarity.to_string() == rarity && i.img_hash == img_hash)
                            .unwrap();
                        // could not download image
                        illust.img_hash = Default::default();
                        *illust.img_path.value_mut(language) = None;
                        illust.similarity_index = 0;
                    }

                    if DEBUG {
                        println!(
                            "{} {set_code} [{rarity}] -> {}",
                            if _adding { "Added" } else { "Updated" },
                            img_unreleased.display()
                        );
                    }

                    imported += 1;
                }

                println!("[{name}] Imported {imported} images from sheet ({skipped} skipped)");
            }
        });
}

// Replaces the hash of poor quality sheet images with the hash of the official image.
// Matched on the perceptual hash (H32), which is computed the same way since v2.
fn fix_image_hash(img_hash: &mut String) {
    let perceptual = |hash: &str| {
        hash.split('|')
            .find_map(|part| part.strip_prefix("H32="))
            .map(str::to_owned)
    };
    let Some(current) = perceptual(img_hash) else {
        return;
    };
    let fixed = FIXED_IMAGE_HASHES
        .iter()
        .find(|(poor, _)| perceptual(poor).as_deref() == Some(current.as_str()));
    if let Some((_, good)) = fixed {
        *img_hash = good.to_string();
    }
}

const FIXED_IMAGE_HASHES: &[(&str, &str)] = &[
    // hBP05-079 (P) Miko, I'm ashamed - poor quality
    (
        "v2|H32=Uq2rEtSqKHWrYtRzqkxrnfVaVK0rZ5XivFIrrWES1IhqFZLtVKosmqpULZqqV62KcnirZJJFmyxGk2pVcLhSlY6HUlU|CYM=0C0F1C",
        "v2|H32=QpWjGlRVKmWrqtRC6qhqnfUK1a6rrhRnlOortasa1BqrrDJFBUVsmjhyLJqHB62aY9it6NFsjS6Gh63qcHgtio+HLos|CYM=0B101F",
    ),
    // hBP04-002 (P) Juufuutei Raden - poor quality
    (
        "v2|H32=qqaVqq6qrXZtVWpVZVVKSVJXZqtaSvaqXFwmksxWbRXKRdqKLUrWUqVVVNvtiyZdti2Spxpk0RaStNKllnbSZJakkrQ|CYM=022450",
        "v2|H32=7CooXeakanWJoVBFUitcy2dbe6sO2WU7KFvVqipS0aqOVFSrTU6lbcbWTapp262KNlAqVWvaKlwqempdrSqqVZWqalM|CYM=121F11",
    ),
    // hBP04-013 (P) Hakui Koyori - poor quality
    (
        "v2|H32=qk1rkrrMrwoS3Fq6WotavUi6EJXEupSahBukVkyTWVdsN5qlsktaZSXbtLoqRaZVNWiWViJZtSpm3SYLiTTUoqp6yyY|CYM=042F54",
        "v2|H32=vKqlapCqtQ4nb1qcWk1a6xpV0maLtIR2pKSpuq2vRaupqtolxhYrk+2lLJo20aSaPNhRl6qySdUkiy3ZlNaSlqml0lo|CYM=062619",
    ),
    // hBP03-105 (U) Lu-knights - not enough cropping
    (
        "v2|H32=nqaPopSmoaujSyRZJNlm2WZVblVqVWpValRefN9s1CpWKhJdB5wvig74rYvAfqkq8DHpOvqA4fjgYeG66IPgeP4D+hg|CYM=2E0B08",
        "v2|H32=vKK/plKlAyupq6QVoNUl2Sf5amVqRahZulxKdW0o1CrSOrJJJ2YnmyeyjZvATMyu5DhsZvwzbGaYmYxp2M7IbOwHbDI|CYM=360C09",
    ),
    // hBP02-054 (P) Mori Calliope - poor quality
    (
        "v2|H32=ClWJTlpVMjV1UnQ6NPOkUmyVqtcs1UapW9VG7Vs7SudPCZJWnKombU/V8HGgKlWqnxZNVYpXTWViSVmWW6tVpGhNbUU|CYM=094937",
        "v2|H32=qqqNKtbV8yNyajQ5tGoEuuxUo9MsDUpVyItmJUvVVjWJlZpatKlmtWtVdLJYoxyqzxamrNOXJG2iFCyKWKNU2ql0aq0|CYM=035962",
    ),
    // hBP01-007 (OUR) Hoshimachi Suisei - not enough cropping, sample
    (
        "v2|H32=RO2wXOHBFD6qoyzaWilMUkNZTaxGV6mKVqWloru8FKfjXFRr0rSptlVSWdNVa02WUMtJtapUilYqS6Cm1aImrdWqUm0|CYM=4B3901",
        "v2|H32=1EysVLHFkD4qgSyzs2nM2EtRVYxEV2maVLetontUlKLrVFRrkFWrtlFbXdJFa001WKVK1apWi1arWipVqaQ2TbWiWmU|CYM=544202",
    ),
    // hBP09-001 (P) Oozora Subaru - sample with a washed out emblem, closer to the TOP 8
    // variant than to the CHAMPION one (golden name) it shows
    (
        "v3|H32=TNmvK9vQpSWTJNmka6xarVLKGc+2SpPSsmWkkZPlLnHxKFRKVpUrpaVSIFVKJS1p+dCJokVrgVaFqvWo1daV2pZqkqQ|CYM=052C75",
        "v3|H32=TNsvEdPSpTWzJMk0S6xarWrOGeq2UrPSsnUlsZvVNDNpOVZKVpUrpKVSNPR6Ja1tadSLokWrqXatqqWllbIV24dq0qQ|CYM=053081",
    ),
];

pub fn retrieve_qna_from_ogbajoj_sheet(all_qnas: &mut QnaDatabase, cache_dir: Option<&Path>) {
    println!("Retrieve all Q&As info from @ogbajoj's sheet");

    let Some(spreadsheet) = retrieve_spreadsheet(cache_dir) else {
        eprintln!("Warning: unable to retrieve spreadsheet metadata; skipping Q&A import");
        return;
    };

    let sheets_gid = spreadsheet
        .sheets
        .iter()
        .filter(|s| s.properties.title.contains("Q&A"))
        .map(|s| s.properties.sheet_id)
        .collect_vec();
    // dbg!(&sheets_gid);

    let mut updated_count = 0;

    // Precompute selectors
    let table_sel = Selector::parse("table").unwrap();
    let tr_sel = Selector::parse("tr").unwrap();
    let td_sel = Selector::parse("td").unwrap();

    for gid in sheets_gid {
        let Some(html) = retrieve_sheet_html(gid, cache_dir) else {
            eprintln!("Warning: failed to retrieve Q&A sheet gid={gid}; skipping");
            continue;
        };
        // fs::write(format!("sheet_{gid}.html"), &html).unwrap();

        let document = Html::parse_document(&html);
        if let Some(table) = document.select(&table_sel).next() {
            let rows_text: Vec<String> = table
                .select(&tr_sel)
                .map(|tr| {
                    let tds = tr.select(&td_sel).collect_vec();
                    extract_cell_text(&tds, Some(0))
                })
                .filter(|s| !s.is_empty())
                .collect();

            for (question, answer) in rows_text.iter().tuple_windows() {
                if !question.starts_with('Q') || !answer.starts_with('A') {
                    continue;
                }

                let Some((number, question)) = question.split_once('-') else {
                    eprintln!("Invalid question format: {}", question);
                    continue;
                };
                let Some((_, answer)) = answer.split_once('-') else {
                    eprintln!("Invalid answer format: {}", answer);
                    continue;
                };

                let number = number.trim().to_string();
                let question = question.trim().to_string();
                let answer = answer.trim().to_string();

                let Some(qna) = all_qnas.get_mut(&number.into()) else {
                    // println!("Q&A {} not found", number);
                    continue;
                };
                qna.question.english = Some(question);
                qna.answer.english = Some(answer);
                updated_count += 1;
            }
        }
    }

    println!("Updated {updated_count} Q&As");

    let missing_english = all_qnas
        .values_mut()
        .filter(|c| c.question.english.is_none())
        .count();
    println!("Missing english questions: {missing_english}");
    let missing_english = all_qnas
        .values_mut()
        .filter(|c| c.answer.english.is_none())
        .count();
    println!("Missing english answers: {missing_english}");
}

fn should_retry_transient_status(status: StatusCode) -> bool {
    status == StatusCode::REQUEST_TIMEOUT
        || status == StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}

fn should_retry_reqwest_error(err: &reqwest::Error) -> bool {
    err.is_timeout()
        || err.is_connect()
        || err.is_request()
        || err.is_body()
        || err.status().is_some_and(should_retry_transient_status)
}

fn retry_backoff(attempt: u32) -> Duration {
    let seconds = 2_u64.pow(attempt.saturating_sub(1).min(3));
    Duration::from_secs(seconds)
}

fn fetch_text_with_retry(
    context: &str,
    mut request: impl FnMut() -> Result<reqwest::blocking::Response, reqwest::Error>,
) -> Option<String> {
    for attempt in 1..=GOOGLE_FETCH_MAX_ATTEMPTS {
        let is_last_attempt = attempt == GOOGLE_FETCH_MAX_ATTEMPTS;

        let response = match request() {
            Ok(response) => response,
            Err(err) => {
                if should_retry_reqwest_error(&err) && !is_last_attempt {
                    eprintln!(
                        "Warning: {context} request failed (attempt {attempt}/{GOOGLE_FETCH_MAX_ATTEMPTS}): {err}"
                    );
                    sleep(retry_backoff(attempt));
                    continue;
                }

                eprintln!(
                    "Warning: {context} request failed (attempt {attempt}/{GOOGLE_FETCH_MAX_ATTEMPTS}): {err}"
                );
                return None;
            }
        };

        let status = response.status();
        if !status.is_success() {
            if should_retry_transient_status(status) && !is_last_attempt {
                eprintln!(
                    "Warning: {context} HTTP {status} (attempt {attempt}/{GOOGLE_FETCH_MAX_ATTEMPTS})"
                );
                sleep(retry_backoff(attempt));
                continue;
            }

            eprintln!(
                "Warning: {context} HTTP {status} (attempt {attempt}/{GOOGLE_FETCH_MAX_ATTEMPTS})"
            );
            return None;
        }

        match response.text() {
            Ok(content) => return Some(content),
            Err(err) => {
                if should_retry_reqwest_error(&err) && !is_last_attempt {
                    eprintln!(
                        "Warning: {context} read failed (attempt {attempt}/{GOOGLE_FETCH_MAX_ATTEMPTS}): {err}"
                    );
                    sleep(retry_backoff(attempt));
                    continue;
                }

                eprintln!(
                    "Warning: {context} read failed (attempt {attempt}/{GOOGLE_FETCH_MAX_ATTEMPTS}): {err}"
                );
                return None;
            }
        }
    }

    None
}

fn retrieve_spreadsheet(cache_dir: Option<&Path>) -> Option<Spreadsheet> {
    let api_key = std::env::var("GOOGLE_SHEETS_API_KEY").expect("GOOGLE_SHEETS_API_KEY not set");

    let content = get_cached_or_download_text(
        cache_dir,
        Path::new("ogbajoj_sheet_cache").join("spreadsheet.json"),
        || {
            let url = format!("https://sheets.googleapis.com/v4/spreadsheets/{SPREADSHEET_ID}");
            fetch_text_with_retry("spreadsheet metadata", || {
                google_sheets_api_http_client()
                    .get(&url)
                    .query(&[("key", api_key.as_str())])
                    .send()
            })
        },
    )?;

    let spreadsheet: Spreadsheet = match serde_json::from_str(&content) {
        Ok(spreadsheet) => spreadsheet,
        Err(err) => {
            eprintln!("Warning: failed to parse spreadsheet metadata: {err}");
            return None;
        }
    };
    // dbg!(&spreadsheet);

    Some(spreadsheet)
}

fn extract_cell_text(tds: &[scraper::ElementRef], idx: Option<usize>) -> String {
    idx.and_then(|idx| tds.get(idx))
        .map(|td| {
            let mut result = String::new();
            for node in td.descendants() {
                if let Some(element) = node.value().as_element() {
                    if element.name() == "br" {
                        result.push('\n');
                    }
                } else if let Node::Text(text) = node.value() {
                    result.push_str(text);
                }
            }
            result.trim().to_string()
        })
        .unwrap_or_default()
}

fn retrieve_spreadsheet_data(sheet: &Sheet, cache_dir: Option<&Path>) -> Option<Vec<SheetCard>> {
    // Precompute selectors
    let table_sel = Selector::parse("table").unwrap();
    let tr_sel = Selector::parse("tr").unwrap();
    let td_sel = Selector::parse("td").unwrap();
    let th_sel = Selector::parse("th[id]").unwrap();
    let img_sel = Selector::parse("img").unwrap();

    let name = sheet.properties.title.clone();
    let sheet_id = sheet.properties.sheet_id;

    // Read HTML content from the website
    let html = retrieve_sheet_html(sheet_id, cache_dir)?;

    println!("[{name}] Reading HTML...");

    let document = Html::parse_document(&html);
    if document.select(&table_sel).next().is_some() {
        // Large tabs can come split in several tables: the rows of all of them, in order
        let trs = document.select(&tr_sel).collect_vec();
        let row_id_prefix = format!("{sheet_id}R");
        // Build header index map from the first tbody row of <td> labels
        let mut set_code_idx: Option<usize> = None;
        let mut card_name_idx: Option<usize> = None;
        let mut image_idx: Option<usize> = None;
        let mut card_type_idx: Option<usize> = None;
        let mut rarity_1_idx: Option<usize> = None;
        let mut color_idx: Option<usize> = None;
        let mut life_hp_idx: Option<usize> = None;
        let mut tags_idx: Option<usize> = None;
        let mut baton_pass_idx: Option<usize> = None;
        let mut text_idx = None;
        let mut alternate_art_idx: Option<usize> = None;
        let mut rarity_2_idx: Option<usize> = None;
        let mut alternate_art_2_idx: Option<usize> = None;
        let mut rarity_3_idx: Option<usize> = None;
        let mut source_idx = None;
        let mut pack_idx = None;
        let mut header_row_idx: Option<usize> = None;

        let mut cards = Vec::with_capacity(trs.len());

        for (i, header_tr) in trs.iter().enumerate() {
            set_code_idx = None;
            card_name_idx = None;
            image_idx = None;
            card_type_idx = None;
            rarity_1_idx = None;
            color_idx = None;
            life_hp_idx = None;
            tags_idx = None;
            baton_pass_idx = None;
            alternate_art_idx = None;
            rarity_2_idx = None;
            alternate_art_2_idx = None;
            rarity_3_idx = None;
            text_idx = None;
            source_idx = None;
            pack_idx = None;

            let headers = header_tr
                .select(&td_sel)
                .map(|td| td.text().collect::<String>().trim().to_ascii_lowercase())
                .collect_vec();
            for (idx, h) in headers.iter().enumerate() {
                if h.contains("setcode") || h.contains("set code") {
                    set_code_idx.get_or_insert(idx);
                }

                if h.contains("card name") {
                    card_name_idx.get_or_insert(idx);
                }

                if h.contains("image") {
                    image_idx.get_or_insert(idx);
                }

                if h.contains("type") {
                    card_type_idx.get_or_insert(idx);
                }

                if h.contains("color") {
                    color_idx.get_or_insert(idx);
                }

                if h.contains("life") || h.contains("hp") {
                    life_hp_idx.get_or_insert(idx);
                }

                if h.contains("tags") && !h.contains("meta") {
                    tags_idx.get_or_insert(idx);
                }

                if h.contains("baton pass") {
                    baton_pass_idx.get_or_insert(idx);
                }

                if h.contains("alternate art") {
                    if h.contains("2") {
                        alternate_art_2_idx.get_or_insert(idx);
                    } else {
                        alternate_art_idx.get_or_insert(idx);
                    }
                }

                if h.contains("rarity") {
                    if rarity_1_idx.is_none() {
                        // first rarity column
                        rarity_1_idx.get_or_insert(idx);
                    } else if rarity_2_idx.is_none() {
                        // second rarity column
                        rarity_2_idx.get_or_insert(idx);
                    } else if rarity_3_idx.is_none() {
                        // third rarity column
                        rarity_3_idx.get_or_insert(idx);
                    }
                }

                if h.contains("text") {
                    text_idx.get_or_insert(idx);
                }

                if h.contains("source") {
                    source_idx.get_or_insert(idx);
                }

                if h == "pack" {
                    pack_idx.get_or_insert(idx);
                }
            }

            // dbg!(set_code_idx, image_idx, rarity_1_idx, text_idx);

            if set_code_idx.is_some() && image_idx.is_some() && rarity_1_idx.is_some() {
                header_row_idx = Some(i);

                if DEBUG {
                    println!(
                        "Header indices -> setcode: {:?}, image: {:?}, rarity: {:?}",
                        set_code_idx, image_idx, rarity_1_idx
                    );
                }
                break;
            }
        }

        // If we don't find headers, skip this table
        let Some(header_row_idx) = header_row_idx else {
            println!("[{name}] missing header row");
            return None;
        };

        // Process data rows after the header row
        for tr in trs.into_iter().skip(header_row_idx + 1) {
            // the row of the sheet, from the row header id "<sheet>R<0-based row>"
            let Some(row_idx) = tr
                .select(&th_sel)
                .find_map(|th| {
                    th.value()
                        .attr("id")?
                        .strip_prefix(&row_id_prefix)?
                        .parse::<usize>()
                        .ok()
                })
                .map(|row| row + 1)
            else {
                // e.g. the frozen rows bar
                continue;
            };

            let tds = tr.select(&td_sel).collect_vec();
            if tds.is_empty()
                || tds
                    .iter()
                    .all(|td| td.text().collect::<String>().trim().is_empty())
            {
                continue;
            }

            let set_code = extract_cell_text(&tds, set_code_idx);
            if set_code.is_empty() {
                if DEBUG {
                    println!("[{name}] row {row_idx}: missing set code");
                }
                continue;
            }

            let card_name = extract_cell_text(&tds, card_name_idx);
            let card_type = extract_cell_text(&tds, card_type_idx);
            let color = extract_cell_text(&tds, color_idx);
            let life_hp = extract_cell_text(&tds, life_hp_idx);
            let tags = extract_cell_text(&tds, tags_idx);
            let baton_pass = extract_cell_text(&tds, baton_pass_idx);
            let text = extract_cell_text(&tds, text_idx);
            let source = extract_cell_text(&tds, source_idx);
            let pack = pack_idx.map(|idx| extract_cell_text(&tds, Some(idx)));

            // language switch for English exclusive cards like hY01-008
            let language = if source.contains("(JP)") {
                Language::Japanese
            } else if text.contains("EN exclusive")
                || source.contains("EN only")
                || source.contains("(EN)")
                || text.contains("available in EN")
            {
                Language::English
            } else {
                Language::Japanese
            };

            // Collect image sources and rarities
            let mut images = Vec::with_capacity(2);

            // main image
            let img_src = image_idx
                .and_then(|idx| tds.get(idx))
                .and_then(|td| td.select(&img_sel).next())
                .and_then(|img| img.value().attr("src"))
                .map(|s| s.to_string());
            let rarity_1 = rarity_1_idx
                .and_then(|idx| tds.get(idx))
                .map(|td| td.text().collect::<String>().trim().to_string());
            if let Some(img_src) = img_src
                && let Some(rarity) = rarity_1
            {
                images.push((img_src, rarity, image_idx.unwrap()));
            }

            // first alternate art
            let rarity_2 = rarity_2_idx
                .and_then(|idx| tds.get(idx))
                .map(|td| td.text().collect::<String>().trim().to_string())
                .filter(|r| r != "SY"); // Cheers have a different number
            let alternate_art_src = alternate_art_idx
                .and_then(|idx| tds.get(idx))
                .and_then(|td| td.select(&img_sel).next())
                .and_then(|img| img.value().attr("src"))
                .map(|s| s.to_string());
            if let Some(alt_art_src) = alternate_art_src
                && let Some(rarity) = rarity_2
            {
                images.push((alt_art_src, rarity, alternate_art_idx.unwrap()));
            }

            // second alternate art
            let rarity_3 = rarity_3_idx
                .and_then(|idx| tds.get(idx))
                .map(|td| td.text().collect::<String>().trim().to_string())
                .filter(|r| r != "SY"); // Cheers have a different number
            let alternate_art_2_src = alternate_art_2_idx
                .and_then(|idx| tds.get(idx))
                .and_then(|td| td.select(&img_sel).next())
                .and_then(|img| img.value().attr("src"))
                .map(|s| s.to_string());
            if let Some(alt_art_src) = alternate_art_2_src
                && let Some(rarity) = rarity_3
            {
                images.push((alt_art_src, rarity, alternate_art_2_idx.unwrap()));
            }

            for (img_src, rarity, col_idx) in images {
                let col_idx = col_idx + 1; // 1-based index

                if DEBUG {
                    println!("[{name}] row: {set_code} -> {img_src}");
                }

                // one image for hashing, the other for display
                let query = img_src.find("?").map(|idx| &img_src[idx..]).unwrap_or("");
                let full_size_img_url = img_src
                    .find("=")
                    .map(|idx| &img_src[..idx])
                    .unwrap_or(&img_src);
                let hash_img_ratio = 3;
                let hash_img_w = 40 * hash_img_ratio;
                let hash_img_h = 56 * hash_img_ratio;
                let hash_img_url =
                    format!("{full_size_img_url}=w{hash_img_w}-h{hash_img_h}{query}");
                let full_size_img_url = format!("{full_size_img_url}{query}");

                let card = SheetCard {
                    sheet_id,
                    row_idx,
                    col_idx,
                    set_code: set_code.clone(),
                    card_name_jp_en: card_name.clone(),
                    language,
                    images_src: img_src.clone(),
                    full_size_img_url: full_size_img_url.to_string(),
                    hash_img_url,
                    card_type: card_type.clone(),
                    rarity,
                    color: color.clone(),
                    life_hp: life_hp.clone(),
                    tags: tags.clone(),
                    baton_pass: baton_pass.clone(),
                    text: text.clone(),
                    source: source_idx.map(|_| source.clone()),
                    pack: pack.clone(),
                };

                if DEBUG {
                    println!(
                        "[{name}] row {row_idx} {set_code}: parsed card: {:#?}",
                        card
                    );
                }

                cards.push(card);
            }
        }

        println!("[{name}] found {} cards", cards.len());
        return Some(cards);
    }

    println!("[{name}] no table found");
    None
}

fn retrieve_sheet_html(sheet_id: u64, cache_dir: Option<&Path>) -> Option<String> {
    let url = format!("https://docs.google.com/spreadsheets/d/{SPREADSHEET_ID}/htmlembed");

    get_cached_or_download_text(
        cache_dir,
        Path::new("ogbajoj_sheet_cache").join(format!("sheet_{sheet_id}.html")),
        || {
            fetch_text_with_retry(&format!("sheet html gid={sheet_id}"), || {
                google_docs_http_client()
                    .get(&url)
                    .query(&[
                        ("gid", sheet_id.to_string().as_str()),
                        ("widget", "false"),
                        ("single", "true"),
                    ])
                    .header("Sec-Fetch-Dest", "document")
                    .header("Sec-Fetch-Mode", "navigate")
                    .header("Sec-Fetch-Site", "none")
                    .header("Sec-Fetch-User", "?1")
                    .send()
            })
        },
    )
}

fn get_cached_or_download_text(
    cache_dir: Option<&Path>,
    cache_path: impl AsRef<Path>,
    mut download: impl FnMut() -> Option<String>,
) -> Option<String> {
    let cache_path = cache_dir.map(|cache_dir| cache_dir.join(cache_path));

    if let Some(cache_path) = cache_path.as_ref()
        && let Ok(content) = fs::read_to_string(cache_path)
    {
        return Some(content);
    }

    let content = download()?;

    if let Some(cache_path) = cache_path {
        if let Some(parent) = cache_path.parent()
            && let Err(err) = fs::create_dir_all(parent)
        {
            eprintln!(
                "Warning: failed to create sheet cache directory {}: {err}",
                parent.display()
            );
            return Some(content);
        }
        if let Err(err) = fs::write(&cache_path, &content) {
            eprintln!(
                "Warning: failed to write sheet cache file {}: {err}",
                cache_path.display()
            );
        }
    }

    Some(content)
}

// Image URLs are external; fetch via HTTP
fn download_sheet_image(
    url: &str,
    row_idx: usize,
    set_code: &str,
    name: &str,
) -> Result<DynamicImage, Box<dyn Error>> {
    let resp = google_docs_http_client()
        .get(url)
        .header(REFERER, "https://docs.google.com/")
        .header("Sec-Fetch-Dest", "image")
        .header("Sec-Fetch-Mode", "no-cors")
        .header("Sec-Fetch-Site", "cross-site")
        .send()
        .inspect_err(|e| {
            eprintln!("[{name}] row {row_idx} {set_code}: request error: {e}");
        })?;

    if !resp.status().is_success() {
        eprintln!(
            "[{name}] row {row_idx} {set_code}: HTTP status {} for {url}",
            resp.status()
        );
        return Err(Box::from(format!("HTTP error: {}", resp.status())));
    }
    let img_bytes = resp.bytes().inspect_err(|e| {
        eprintln!("[{name}] row {row_idx} {set_code}: read body error: {e}");
    })?;

    // Decode image
    Ok(image::load_from_memory(&img_bytes).inspect_err(|e| {
        eprintln!("[{name}] row {row_idx} {set_code}: decode error: {e}");
    })?)
}

// --- Promo sources and product names ---

/// Fills from the sheet:
/// - `promo_sources` of the printings, from the "Source(s)" column of the rows of their sheet
///   cells (`ogbajoj_sheet_cells`), and the PR packs they are in (the "PR Pack contents" tabs);
/// - `products` of the unreleased printings, from the tabs of their rows, see
///   [`update_unreleased_products`];
/// - the unofficial english names of the products the official site doesn't name in english,
///   from the "Pack" column and the tab titles.
///
/// Tabs are recognized by their header row, not by name. Printings or products whose tabs
/// couldn't be read keep their values.
pub fn retrieve_product_info_from_ogbajoj_sheet(
    all_cards: &mut CardsDatabase,
    products: &mut ProductsDatabase,
    grids: &ImageGrids,
    cache_dir: Option<&Path>,
) {
    println!("Retrieving promo sources and product names from @ogbajoj's sheet...");

    if std::env::var("GOOGLE_SHEETS_API_KEY").is_err() {
        eprintln!(
            "Warning: GOOGLE_SHEETS_API_KEY not set; keeping existing promo sources and names"
        );
        return;
    }
    let Some(spreadsheet) = retrieve_spreadsheet(cache_dir) else {
        eprintln!(
            "Warning: unable to retrieve spreadsheet metadata; keeping existing promo sources and names"
        );
        return;
    };

    let pr_packs = read_pr_packs(all_cards, &spreadsheet, grids, cache_dir);

    // the html of the tabs is cached, the other sheet steps already read them
    let tabs: Vec<(&Sheet, Option<Vec<SheetCard>>)> = spreadsheet
        .sheets
        .par_iter()
        .filter(|sheet| pr_pack_tab_language(&sheet.properties.title).is_none())
        .map(|sheet| (sheet, retrieve_spreadsheet_data(sheet, cache_dir)))
        .collect();

    // --- promo sources ---
    let unread_tabs: HashSet<SheetId> = tabs
        .iter()
        .filter(|(_, cards)| cards.is_none())
        .map(|(sheet, _)| sheet.properties.sheet_id)
        .collect();
    let mut sources: SourceTexts = HashMap::new();
    for card in tabs
        .iter()
        .filter_map(|(_, cards)| cards.as_ref())
        .flatten()
    {
        sources.insert(
            (card.sheet_id, card.row_idx, card.col_idx),
            (card.set_code.as_str(), card.source.as_deref()),
        );
    }
    let source_tabs: HashSet<SheetId> = sources
        .iter()
        .filter(|(_, (_, source))| source.is_some())
        .map(|((sheet, _, _), _)| *sheet)
        .collect();
    if source_tabs.is_empty() {
        eprintln!(
            "Warning: no tab with a \"Source\" column; keeping existing promo sources, adding the PR packs"
        );
    }
    let (updated_count, kept_count, shared_count, moved_count) = update_promo_sources(
        all_cards,
        (!source_tabs.is_empty()).then_some((&sources, &unread_tabs)),
        &pr_packs,
    );
    println!(
        "Promo sources: {} tabs with a source column, updated {updated_count} illustrations ({shared_count} from the row of a variant, {kept_count} kept, their tab couldn't be read)",
        source_tabs.len()
    );
    if moved_count > 0 {
        eprintln!(
            "Warning: {moved_count} illustrations kept their promo sources, their sheet cell is now another card's row (run --ogbajoj-sheet to update the cells)"
        );
    }

    // --- products of the unreleased printings ---
    let (updated_count, kept_count) = update_unreleased_products(all_cards, products, &tabs);
    println!(
        "Unreleased products: updated {updated_count} illustrations ({kept_count} kept, their row couldn't be read)"
    );

    // --- product names ---
    let mut pack_lines: HashMap<String, BTreeSet<String>> = HashMap::new();
    for card in tabs
        .iter()
        .filter_map(|(_, cards)| cards.as_ref())
        .flatten()
    {
        for line in card.pack.iter().flat_map(|pack| pack.lines()) {
            let (line, _) = split_language_tag(line);
            if !line.is_empty() {
                pack_lines
                    .entry(card.set_code.clone())
                    .or_default()
                    .insert(line.to_string());
            }
        }
    }
    for (sheet, cards) in &tabs {
        let count = cards
            .iter()
            .flatten()
            .filter(|c| c.pack.is_some())
            .map(|c| &c.set_code)
            .unique()
            .count();
        if count > 0 {
            println!(
                "[{}] \"Pack\" column for {count} cards",
                sheet.properties.title
            );
        }
    }
    if pack_lines.is_empty() {
        eprintln!("Warning: no tab with a \"Pack\" column; keeping existing product names");
        return;
    }
    let titles = spreadsheet
        .sheets
        .iter()
        .map(|sheet| sheet.properties.title.clone())
        .collect_vec();
    let names = update_unofficial_names(products, all_cards, &pack_lines, &titles);
    for (code, name) in &names {
        match name {
            UnofficialName::Official => {}
            UnofficialName::Pack(name) => println!("Product {code}: {name:?} (Pack column)"),
            UnofficialName::TabTitle(name) => println!("Product {code}: {name:?} (tab title)"),
            UnofficialName::Source(name) => {
                println!("Product {code}: {name:?} (Source(s) of its promos)")
            }
            UnofficialName::None(best) => println!(
                "Product {code}: no english name{}",
                best.as_ref()
                    .map(|(line, precision, recall)| format!(
                        ", best Pack line {line:?} is on {:.0}% of its cards and {:.0}% of the cards with it",
                        recall * 100.0,
                        precision * 100.0
                    ))
                    .unwrap_or_default()
            ),
        }
    }

    // the short names follow the sheet's names
    update_short_names(products);

    // --- sources that are the name of the printing's product ---
    let dropped_count = drop_product_sources(all_cards, products);
    println!("Promo sources: dropped {dropped_count} that are the name of their product");
}

/// Sets `promo_sources` from the "Source(s)" text of the sheet rows of each printing (`sources`,
/// by sheet cell). The sheet has one row per card number and kind of promo: prize variants
/// (champion, top 4, top 8, ... with the same art) and the english print of a promo share the row
/// matched to one of them. A printing without a row of its own takes the row of the closest
/// printing of the same card and rarity that has one, when their images are within the same
/// rarity tolerance (on the sheet (2026-11), the variants are 42 to 113 apart, different art 171
/// or more). Then the PR packs of the printing (`pr_packs`) are added, unless a source line
/// already names them (written differently, e.g. "vol.1" / "vol 1").
///
/// What couldn't be read keeps the stored sources, with only additions: all the sources when
/// `sources` is `None` (no tab with a source column), a printing with a cell in an unread tab
/// (`unread_tabs`) or a cell that is now the row of another card (the sheet changed since
/// `ogbajoj_sheet_cells` was updated), and a language whose PR packs are `None`.
/// Returns the number of updated, kept, shared and kept because of a moved row printings.
fn update_promo_sources(
    all_cards: &mut CardsDatabase,
    sources: Option<(&SourceTexts, &HashSet<SheetId>)>,
    pr_packs: &PrPacks,
) -> (usize, usize, usize, usize) {
    let mut updated_count = 0;
    let mut kept_count = 0;
    let mut shared_count = 0;
    let mut moved_count = 0;
    for card in all_cards.values_mut() {
        // the source texts of each printing's own rows, None when a tab couldn't be read
        let own: Vec<Option<Vec<&str>>> = card
            .illustrations
            .iter()
            .map(|illust| {
                let (sources, unread_tabs) = sources?;
                let cells = illust.ogbajoj_sheet_cells.iter().flatten().collect_vec();
                if cells.iter().any(|(sheet, _)| unread_tabs.contains(sheet)) {
                    return None;
                }
                let mut texts = vec![];
                for (sheet, cell) in cells {
                    let Some((row, col)) = cell.to_row_col() else {
                        continue;
                    };
                    match sources.get(&(*sheet, row, col)) {
                        Some((number, _)) if number.trim() != illust.card_number => {
                            moved_count += 1;
                            return None;
                        }
                        Some((_, Some(text))) => texts.push(*text),
                        _ => {}
                    }
                }
                Some(texts)
            })
            .collect();

        let texts: Vec<Option<(Vec<&str>, bool)>> = card
            .illustrations
            .iter()
            .enumerate()
            .map(|(i, illust)| {
                let own_texts = own[i].clone()?;
                if !own_texts.is_empty() || illust.img_hash.is_empty() {
                    return Some((own_texts, false));
                }
                // the closest variant with a row of its own (or that couldn't be read: kept)
                let variant = card
                    .illustrations
                    .iter()
                    .enumerate()
                    .filter(|(j, other)| {
                        *j != i
                            && other.rarity == illust.rarity
                            && !other.img_hash.is_empty()
                            && own[*j].as_ref().is_none_or(|t| !t.is_empty())
                    })
                    .map(|(j, other)| (dist_hash(&other.img_hash, &illust.img_hash), j))
                    .filter(|(dist, _)| *dist <= DIST_TOLERANCE_SAME_RARITY)
                    .min();
                Some(match variant {
                    Some((_, j)) => (own[j].clone()?, true),
                    None => (vec![], false),
                })
            })
            .collect();

        for (illust, texts) in card.illustrations.iter_mut().zip(texts) {
            let from_sources = texts.map(|(texts, shared)| {
                let lines = texts
                    .iter()
                    .flat_map(|text| parse_source_lines(text))
                    .unique_by(|(text, _)| text.clone())
                    .collect_vec();
                let promo_sources = promo_sources_of(&lines, &illust.manage_id);
                if shared && promo_sources.has_value() {
                    shared_count += 1;
                }
                promo_sources
            });
            if from_sources.is_none() && sources.is_some() {
                kept_count += 1;
            }
            let key = illust_key(illust);
            let mut promo_sources = Localized::<Vec<String>>::default();
            for language in [Language::Japanese, Language::English] {
                let packs = pr_packs
                    .value(language)
                    .as_ref()
                    .map(|packs| packs.get(&key).cloned().unwrap_or_default());
                let stored = illust.promo_sources.value(language).clone();
                // what can't be read keeps the stored sources
                let mut values = match &from_sources {
                    Some(from_sources) => from_sources.value(language).clone().unwrap_or_default(),
                    None => stored.clone().unwrap_or_default(),
                };
                if packs.is_none() {
                    add_sources(&mut values, stored.into_iter().flatten());
                }
                add_sources(&mut values, packs.into_iter().flatten());
                *promo_sources.value_mut(language) = Some(values).filter(|v| !v.is_empty());
            }
            if illust.promo_sources != promo_sources {
                illust.promo_sources = promo_sources;
                updated_count += 1;
            }
        }
    }
    (updated_count, kept_count, shared_count, moved_count)
}

/// The card number, language and product (see [`row_product`]) of a sheet row.
type SheetRow<'a> = (&'a str, Language, Option<String>);

/// Sets the products of the unreleased printings (no manage id), in the language of their sheet
/// row: the official site lists the products of the released ones only. The
/// product is the one of the row's tab, see [`row_product`]; promo tabs are about no product
/// (the promos have their sources). A product not in the catalog yet (a booster before its card
/// list) is added to it, with the category of the products of the same kind of tab (see
/// [`tab_category`]); the sheet names it. Once the official site lists the printing, its
/// products replace these.
///
/// A printing keeps its products when a tab of its cells couldn't be read, or a cell is now the
/// row of another card. Returns the number of updated and kept printings.
fn update_unreleased_products(
    all_cards: &mut CardsDatabase,
    products: &mut ProductsDatabase,
    tabs: &[(&Sheet, Option<Vec<SheetCard>>)],
) -> (usize, usize) {
    let unread_tabs: HashSet<SheetId> = tabs
        .iter()
        .filter(|(_, cards)| cards.is_none())
        .map(|(sheet, _)| sheet.properties.sheet_id)
        .collect();
    // (tab, row, column) -> card number, language, product
    let mut rows: HashMap<(SheetId, usize, usize), SheetRow> = HashMap::new();
    for (sheet, cards) in tabs {
        let tab_codes = tab_product_codes(&sheet.properties.title);
        for card in cards.iter().flatten() {
            rows.insert(
                (card.sheet_id, card.row_idx, card.col_idx),
                (
                    card.set_code.as_str(),
                    card.language,
                    row_product(card.set_code.trim(), &tab_codes),
                ),
            );
        }
    }

    let mut updated_count = 0;
    let mut kept_count = 0;
    let mut new_products = BTreeSet::new();
    for illust in all_cards
        .values_mut()
        .flat_map(|cs| cs.illustrations.iter_mut())
    {
        let cells = illust.ogbajoj_sheet_cells.iter().flatten().collect_vec();
        if cells.is_empty() {
            continue;
        }
        let mut codes: Localized<Vec<String>> = Localized::default();
        let mut keep = false;
        for (sheet, cell) in cells {
            if unread_tabs.contains(sheet) {
                keep = true;
                break;
            }
            let Some((row, col)) = cell.to_row_col() else {
                continue;
            };
            let Some((number, language, code)) = rows.get(&(*sheet, row, col)) else {
                continue;
            };
            if number.trim() != illust.card_number {
                keep = true;
                break;
            }
            if let Some(code) = code {
                let values = codes.value_mut(*language).get_or_insert_default();
                if !values.contains(code) {
                    values.push(code.clone());
                }
            }
        }
        if keep {
            kept_count += 1;
            continue;
        }
        for language in [Language::Japanese, Language::English] {
            let Some(codes) = codes.value(language) else {
                continue;
            };
            if illust.manage_id.has_value() {
                // released: the official site has its products (in the other language too: the
                // language of the row is only a default)
                continue;
            }
            for code in codes {
                if !products.contains_key(code) {
                    new_products.insert(code.clone());
                }
            }
            if illust.products.value(language).as_ref() != Some(codes) {
                *illust.products.value_mut(language) = Some(codes.clone());
                updated_count += 1;
            }
        }
    }
    for code in new_products {
        println!("Product {code}: not on the official site yet, added from the sheet");
        products.entry(code).or_default();
    }
    // the category of the products the official site doesn't list yet, from their tab
    let titles = tabs
        .iter()
        .map(|(sheet, _)| sheet.properties.title.clone())
        .collect_vec();
    let uncategorized = products
        .iter()
        .filter(|(_, product)| product.category.is_none() && !product.name.has_value())
        .map(|(code, _)| code.clone())
        .collect_vec();
    for code in uncategorized {
        let category = tab_category(&code, products, &titles);
        if let Some(category) = &category {
            println!("Product {code}: category {category:?} (tab title)");
        }
        products.get_mut(&code).unwrap().category = category;
    }
    (updated_count, kept_count)
}

/// The card number and "Source(s)" text (`None` without the column) of the sheet rows, by sheet
/// cell (tab, row, column).
type SourceTexts<'a> = HashMap<(SheetId, usize, usize), (&'a str, Option<&'a str>)>;

/// Adds the sources that are not already in `values`, written the same way or differently
/// (case, spaces, punctuation: "holodori PR Pack vol.1" is "holodori PR Pack vol 1").
fn add_sources(values: &mut Vec<String>, new: impl IntoIterator<Item = String>) {
    for source in new {
        let comparable = comparable_name(&source);
        if !values.iter().any(|v| comparable_name(v) == comparable) {
            values.push(source);
        }
    }
}

// --- PR packs ---

/// Two different illustrations whose distances to a card picture are closer than this can't be
/// told apart: the card is skipped. Distances are `dist_hash` units (1000 = completely different).
/// On the 393 card pictures of the PR pack tabs (2026-11), a match leads the next illustration by
/// 106 at the median and 55 at the 5th percentile; the only lead under 20 was between two plain
/// staff support cards (17), the smallest accepted lead is 23.
const PR_PACK_AMBIGUOUS_MARGIN: u64 = 20;

/// A card picture further than [`DIST_TOLERANCE_SAME_RARITY`] from all the illustrations still
/// matches the closest one up to this distance, when it is far ahead of the next one
/// ([`PR_PACK_FAR_MARGIN`]): the small pictures, with "SAMPLE" over the art. On the PR pack tabs
/// (2026-11, edges trimmed), the furthest such matches are at 143 and 151, ahead by 63 and 70; the
/// pictures of cards not in the database are at 173 or more, ahead by 22 at most.
const PR_PACK_FAR_TOLERANCE: u64 = 160;
const PR_PACK_FAR_MARGIN: u64 = 50;

/// Illustrations about as close to a card picture are told apart by their artwork, see
/// [`art_distance`] (0-255 color units): the closest one has to be ahead of the others by this
/// much. On the PR pack tabs (2026-11), the artwork of a matched card is at 17 or less (one at
/// 28), and the next illustration's at 13 or more; "Manager-chan" is at 6, "holoAN" at 44.
const PR_PACK_ART_MARGIN: f32 = 20.0;

/// The edges of a card picture are trimmed by up to 6% of the card on each side, see
/// [`card_pictures`]. On the PR pack tabs (2026-11), this matches 44 more card pictures
/// (most with a drop shadow), and no other match changes.
const PR_PACK_MAX_TRIM: f32 = 0.06;
const PR_PACK_TRIM_STEPS: u32 = 2;
/// Width / height of a trimmed card picture: a card (63x88 mm, 0.716) give or take 2%.
const PR_PACK_TRIMMED_RATIO: std::ops::RangeInclusive<f32> = 0.70..=0.735;

/// The PR packs of the promo illustrations, by language, see [`read_pr_packs`]. A language is
/// `None` when its packs couldn't be read.
type PrPacks = Localized<BTreeMap<IllustKey, Vec<String>>>;

/// Reads the PR packs of the promo illustrations from the "PR Pack contents" tabs of the sheet
/// (one or more per language, e.g. "JP PR Pack contents", "EN PR Pack contents"). Each tab lists
/// packs as text rows followed by floating images (the pack, and a picture of its cards). The
/// pictures are split into cards, and each card is matched to the closest promo illustration
/// (see [`match_pr_pack_image`]), possibly one not released in the language of the pack yet.
///
/// The packs of a language are only known when all its tabs were read and matched; otherwise
/// they are `None`, with a warning, and the stored sources are kept.
fn read_pr_packs(
    all_cards: &CardsDatabase,
    spreadsheet: &Spreadsheet,
    grids: &ImageGrids,
    cache_dir: Option<&Path>,
) -> PrPacks {
    println!("Retrieving PR packs from @ogbajoj's sheet...");
    let mut all_packs = PrPacks::default();

    // the tabs, by language
    let mut tabs: Vec<(&Sheet, Language)> = vec![];
    for sheet in &spreadsheet.sheets {
        match pr_pack_tab_language(&sheet.properties.title) {
            None => {}
            Some(None) => eprintln!(
                "Warning: [{}] PR pack tab without a JP or EN language; skipped",
                sheet.properties.title
            ),
            Some(Some(language)) => tabs.push((sheet, language)),
        }
    }

    for language in [Language::Japanese, Language::English] {
        let language_tabs = tabs
            .iter()
            .filter(|(_, l)| *l == language)
            .map(|(sheet, _)| *sheet)
            .collect_vec();
        if language_tabs.is_empty() {
            eprintln!("Warning: no PR pack tab for {language:?}; keeping existing promo sources");
            continue;
        }

        let candidates = pr_pack_candidates(all_cards, language);
        let preemptive = pr_pack_preemptive_candidates(all_cards, language);

        // all the tabs of the language, or nothing
        let mut tabs_matches = vec![];
        for sheet in &language_tabs {
            let Some(matches) =
                read_pr_pack_tab(sheet, &candidates, &preemptive, language, grids, cache_dir)
            else {
                break;
            };
            tabs_matches.push(matches);
        }
        if tabs_matches.len() != language_tabs.len() {
            eprintln!(
                "Warning: unable to read all the PR pack tabs for {language:?}; keeping existing promo sources"
            );
            continue;
        }

        let mut pr_packs: BTreeMap<IllustKey, Vec<String>> = BTreeMap::new();
        for (pack, matches) in tabs_matches.iter().flatten() {
            for m in matches {
                if let PrPackMatch::Matched(illust, _) = m {
                    let packs = pr_packs.entry(illust_key(illust)).or_default();
                    if !packs.contains(pack) {
                        packs.push(pack.clone());
                    }
                }
            }
        }
        // a layout or image change on the sheet that breaks the matching shouldn't wipe the packs
        if pr_packs.is_empty() {
            eprintln!("Warning: no card matched for {language:?}; keeping existing promo sources");
            continue;
        }
        println!(
            "PR packs ({language:?}): {} illustrations in a pack",
            pr_packs.len()
        );
        *all_packs.value_mut(language) = Some(pr_packs);
    }
    all_packs
}

/// Whether a tab lists the content of PR packs ("... PR Pack contents ...", any case), and its
/// language from a "JP" or "EN" word of its title. `Some(None)` for a PR pack tab without a
/// language, or with both.
pub fn pr_pack_tab_language(title: &str) -> Option<Option<Language>> {
    if !title.to_lowercase().contains("pr pack contents") {
        return None;
    }
    let words = title
        .split(|c: char| !c.is_alphanumeric())
        .map(|w| w.to_uppercase())
        .collect_vec();
    let jp = words.iter().any(|w| w == "JP");
    let en = words.iter().any(|w| w == "EN");
    Some(match (jp, en) {
        (true, false) => Some(Language::Japanese),
        (false, true) => Some(Language::English),
        _ => None,
    })
}

/// The packs of one tab, with the matches of their cards. `None` if anything couldn't be read.
fn read_pr_pack_tab(
    sheet: &Sheet,
    candidates: &[CardIllustration],
    preemptive: &[CardIllustration],
    language: Language,
    grids: &ImageGrids,
    cache_dir: Option<&Path>,
) -> Option<Vec<(String, Vec<PrPackMatch>)>> {
    let name = &sheet.properties.title;
    let sheet_id = sheet.properties.sheet_id;

    let Some(html) = retrieve_sheet_html(sheet_id, cache_dir) else {
        eprintln!("Warning: [{name}] unable to retrieve the sheet");
        return None;
    };
    let Some(row_heights) = retrieve_row_heights(sheet, cache_dir) else {
        eprintln!("Warning: [{name}] unable to retrieve the row heights");
        return None;
    };
    let layout = parse_pr_pack_sheet(&html, sheet_id, &row_heights);
    for note in &layout.notes {
        println!("[{name}] ignored text row without images: {note:?}");
    }
    if layout.unplaced > 0 {
        eprintln!(
            "Warning: [{name}] {} images couldn't be placed",
            layout.unplaced
        );
        return None;
    }
    if layout.images.is_empty() {
        eprintln!("Warning: [{name}] no pack found");
        return None;
    }
    println!(
        "[{name}] found {} packs, {} images",
        layout.images.iter().map(|i| &i.pack).unique().count(),
        layout.images.len()
    );

    // all the images, or nothing: a pack would lose cards
    let matches: Option<Vec<(String, Vec<PrPackMatch>)>> = layout
        .images
        .into_par_iter()
        .map(|image| {
            let img = download_sheet_image(&image.url, image.row, &image.pack, name).ok()?;
            let matches =
                match_pr_pack_image(&img.into_rgb8(), candidates, preemptive, language, grids);
            Some((image.pack, matches))
        })
        .collect();
    let Some(matches) = matches else {
        eprintln!("Warning: [{name}] unable to download all the images");
        return None;
    };

    let mut matched_count = 0;
    let mut preemptive_count = 0;
    let mut not_found_count = 0;
    for (pack, pack_matches) in &matches {
        for m in pack_matches {
            match m {
                PrPackMatch::Matched(illust, dist) => {
                    matched_count += 1;
                    if illust.manage_id.has_value() && illust.manage_id.value(language).is_none() {
                        preemptive_count += 1;
                    }
                    if DEBUG {
                        println!(
                            "[{name}] {pack}: {} {} ({dist})",
                            illust.card_number, illust.rarity
                        );
                    }
                }
                PrPackMatch::Ambiguous(options) => eprintln!(
                    "Warning: [{name}] {pack}: card picture matches several promos, skipped: {}",
                    options
                        .iter()
                        .map(|(i, d)| format!("{} {:?} ({d})", i.card_number, i.manage_id))
                        .join(", ")
                ),
                PrPackMatch::NotFound => not_found_count += 1,
            }
        }
    }
    for (pack, pack_matches) in matches.iter().into_group_map_by(|(pack, _)| pack.clone()) {
        let found = pack_matches
            .iter()
            .flat_map(|(_, m)| m)
            .filter(|m| matches!(m, PrPackMatch::Matched(..)))
            .count();
        if found == 0 {
            println!("[{name}] {pack}: no card matched");
        }
    }
    println!(
        "[{name}] matched {matched_count} card pictures ({preemptive_count} not released in {language:?} yet), {not_found_count} not matched (pack pictures, cards not in the database)"
    );
    Some(matches)
}

/// Promo illustrations released in the other language only: a pack can be announced with cards
/// not released in its language yet (the english version of a japanese promo). The card pictures
/// that match no illustration printed in the language of the pack (see [`pr_pack_candidates`])
/// are matched to these; the pack is then a source in the language of the pack ahead of the
/// release, that the printing keeps once released.
pub fn pr_pack_preemptive_candidates(
    all_cards: &CardsDatabase,
    language: Language,
) -> Vec<CardIllustration> {
    all_cards
        .values()
        .flat_map(|cs| cs.illustrations.iter())
        .filter(|i| i.rarity == CardRarity::Promo && !i.img_hash.is_empty())
        .filter(|i| i.manage_id.has_value() && i.manage_id.value(language).is_none())
        .cloned()
        .collect()
}

/// Promo illustrations printed in this language: released, or unreleased with an image from the
/// sheet. Proxy images (translations of japanese cards) are not english printings.
pub fn pr_pack_candidates(all_cards: &CardsDatabase, language: Language) -> Vec<CardIllustration> {
    all_cards
        .values()
        .flat_map(|cs| cs.illustrations.iter())
        .filter(|i| i.rarity == CardRarity::Promo && !i.img_hash.is_empty())
        .filter(|i| {
            i.manage_id.value(language).is_some()
                || (!i.manage_id.has_value()
                    && i.img_path
                        .value(language)
                        .as_ref()
                        .is_some_and(|p| !p.starts_with(PROXIES_FOLDER)))
        })
        .cloned()
        .collect()
}

/// Identifies an illustration across the database copy used for matching.
type IllustKey = (String, CardRarity, Localized<BTreeSet<u32>>, String);

/// The key of an illustration, see [`IllustKey`].
fn illust_key(illust: &CardIllustration) -> IllustKey {
    (
        illust.card_number.clone(),
        illust.rarity.clone(),
        illust.manage_id.clone(),
        illust.img_hash.clone(),
    )
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct RowMetadataResponse {
    #[serde(default)]
    sheets: Vec<RowMetadataSheet>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct RowMetadataSheet {
    #[serde(default)]
    data: Vec<RowMetadataGrid>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct RowMetadataGrid {
    #[serde(default)]
    row_metadata: Vec<RowMetadata>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct RowMetadata {
    #[serde(default)]
    pixel_size: u32,
    #[serde(default)]
    hidden_by_user: bool,
    #[serde(default)]
    hidden_by_filter: bool,
}

/// The height in pixels of each row of a tab, as displayed (hidden rows are 0). The sheet html
/// only has the height of resized rows and doesn't count the grid lines; the Sheets API gives the
/// displayed height of every row, which is what the floating images are positioned with.
fn retrieve_row_heights(sheet: &Sheet, cache_dir: Option<&Path>) -> Option<Vec<u32>> {
    let api_key = std::env::var("GOOGLE_SHEETS_API_KEY").ok()?;
    let sheet_id = sheet.properties.sheet_id;
    // the whole tab, by its quoted name
    let range = format!("'{}'", sheet.properties.title.replace('\'', "''"));

    let content = get_cached_or_download_text(
        cache_dir,
        Path::new("ogbajoj_sheet_cache").join(format!("rows_{sheet_id}.json")),
        || {
            let url = format!("https://sheets.googleapis.com/v4/spreadsheets/{SPREADSHEET_ID}");
            fetch_text_with_retry(&format!("row heights gid={sheet_id}"), || {
                google_sheets_api_http_client()
                    .get(&url)
                    .query(&[
                        ("key", api_key.as_str()),
                        ("ranges", range.as_str()),
                        (
                            "fields",
                            "sheets(data(rowMetadata(pixelSize,hiddenByUser,hiddenByFilter)))",
                        ),
                    ])
                    .send()
            })
        },
    )?;
    row_heights_from_json(&content)
}

/// The displayed row heights from a Sheets API `rowMetadata` response, hidden rows are 0.
fn row_heights_from_json(content: &str) -> Option<Vec<u32>> {
    let response: RowMetadataResponse = serde_json::from_str(content)
        .inspect_err(|err| eprintln!("Warning: failed to parse row heights: {err}"))
        .ok()?;
    let heights = response
        .sheets
        .into_iter()
        .flat_map(|s| s.data)
        .flat_map(|d| d.row_metadata)
        .map(|r| {
            if r.hidden_by_user || r.hidden_by_filter {
                0
            } else {
                r.pixel_size
            }
        })
        .collect_vec();
    (!heights.is_empty()).then_some(heights)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrPackImage {
    pub pack: String,
    pub url: String,
    /// 1-based row of the image, for messages
    pub row: usize,
}

#[derive(Debug, Default)]
pub struct PrPackLayout {
    /// The images of the packs, in sheet order.
    pub images: Vec<PrPackImage>,
    /// Text rows without images, e.g. "All cards in PR packs are foil - ...".
    pub notes: Vec<String>,
    /// Images that couldn't be positioned on the sheet.
    pub unplaced: usize,
}

/// The packs of a "PR Pack contents" tab: the text rows, and the floating images positioned with
/// `posObj(sheet, id, row, column, x, y)` and the height of the rows (see
/// [`retrieve_row_heights`]). An image belongs to the closest named text row above its vertical
/// middle. A row is named by its text, without trailing notes in parentheses: a row that is only
/// a note, like "(Entry PR Packs have been discontinued as of vol 4)", is never a pack, so the
/// images below it belong to the pack above. Named rows without images (the first row note) are
/// not packs either.
pub fn parse_pr_pack_sheet(html: &str, sheet_id: SheetId, row_heights: &[u32]) -> PrPackLayout {
    static ROWS: LazyLock<Selector> = LazyLock::new(|| Selector::parse("tr").unwrap());
    static ROW_HEADER: LazyLock<Selector> = LazyLock::new(|| Selector::parse("th[id]").unwrap());
    static CELLS: LazyLock<Selector> = LazyLock::new(|| Selector::parse("td").unwrap());
    static OVERLAYS: LazyLock<Selector> =
        LazyLock::new(|| Selector::parse("div.waffle-embedded-object-overlay[id]").unwrap());
    static IMG: LazyLock<Selector> = LazyLock::new(|| Selector::parse("img[src]").unwrap());

    let document = Html::parse_document(html);
    let row_prefix = format!("{sheet_id}R");

    // (0-based row, text, name)
    let mut text_rows: Vec<(usize, String, String)> = vec![];
    for tr in document.select(&ROWS) {
        let Some(row) = tr
            .select(&ROW_HEADER)
            .next()
            .and_then(|th| th.value().attr("id"))
            .and_then(|id| id.strip_prefix(&row_prefix))
            .and_then(|row| row.parse().ok())
        else {
            continue;
        };
        let cells = tr.select(&CELLS).collect_vec();
        let first_text = cells
            .iter()
            .position(|td| !td.text().collect::<String>().trim().is_empty());
        let text = extract_cell_text(&cells, first_text);
        if !text.is_empty() {
            let name = strip_trailing_notes(&text);
            text_rows.push((row, text, name));
        }
    }
    text_rows.sort_by_key(|(row, _, _)| *row);

    // floating images by element id: (url, displayed height)
    let overlays: HashMap<&str, (&str, Option<i64>)> = document
        .select(&OVERLAYS)
        .filter_map(|div| {
            let id = div.value().attr("id")?;
            let url = div.select(&IMG).next()?.value().attr("src")?;
            let height = div.value().attr("style").and_then(|style| {
                style.split(';').find_map(|decl| {
                    let (key, value) = decl.split_once(':')?;
                    (key.trim() == "height")
                        .then(|| value.trim().trim_end_matches("px").trim().parse().ok())
                        .flatten()
                })
            });
            Some((id, (url, height)))
        })
        .collect();

    // top of each row, in pixels
    let row_tops = row_heights
        .iter()
        .scan(0i64, |top, h| {
            let row_top = *top;
            *top += *h as i64;
            Some(row_top)
        })
        .chain(std::iter::once(row_heights.iter().map(|h| *h as i64).sum()))
        .collect_vec();

    let mut layout = PrPackLayout::default();
    // (row of the pack, top of the image, image)
    let mut images: Vec<(usize, i64, PrPackImage)> = vec![];
    for call in html.split("posObj(").skip(1) {
        let Some((args, _)) = call.split_once(')') else {
            continue;
        };
        let args = args
            .split(',')
            .map(|a| a.trim().trim_matches('\''))
            .collect_vec();
        // also skips the function definition: posObj(sheet, id, row, col, x, y)
        let [sheet, id, row, _col, _x, y] = args[..] else {
            continue;
        };
        if sheet != sheet_id.to_string() {
            continue;
        }
        let (Ok(row), Ok(y)) = (row.parse::<usize>(), y.parse::<i64>()) else {
            continue;
        };
        let Some((url, height)) = overlays.get(id) else {
            continue;
        };
        let Some(row_top) = row_tops.get(row).filter(|_| row < row_heights.len()) else {
            layout.unplaced += 1;
            continue;
        };
        let top = row_top + y;
        let middle = top + height.unwrap_or(0) / 2;
        // the row of the middle of the image
        let Some(image_row) = row_tops
            .windows(2)
            .position(|w| w[0] <= middle && middle < w[1])
        else {
            layout.unplaced += 1;
            continue;
        };
        // the closest named row above, notes in parentheses are skipped
        let Some((pack_row, _, pack)) = text_rows
            .iter()
            .rfind(|(r, _, name)| *r <= image_row && !name.is_empty())
        else {
            println!("Image above the first pack, ignored: {url}");
            continue;
        };
        images.push((
            *pack_row,
            top,
            PrPackImage {
                pack: pack.clone(),
                url: url.to_string(),
                row: image_row + 1,
            },
        ));
    }

    // sheet order
    images.sort_by_key(|(pack_row, top, _)| (*pack_row, *top));
    layout.notes = text_rows
        .iter()
        .filter(|(row, _, _)| !images.iter().any(|(r, _, _)| r == row))
        .map(|(_, text, _)| text.clone())
        .collect();
    layout.images = images.into_iter().map(|(_, _, image)| image).collect();
    layout
}

/// The text without its trailing notes in parentheses, e.g. "Mikodanye PR Pack (cards in this
/// pack are not foiled)" -> "Mikodanye PR Pack". Empty when the whole text is a note.
pub fn strip_trailing_notes(text: &str) -> String {
    let mut text = text.trim();
    while text.ends_with(')') {
        // the matching opening parenthesis
        let mut depth = 0;
        let mut start = None;
        for (i, c) in text.char_indices().rev() {
            match c {
                ')' => depth += 1,
                '(' => {
                    depth -= 1;
                    if depth == 0 {
                        start = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(start) = start else {
            break;
        };
        text = text[..start].trim_end();
    }
    text.split_whitespace().join(" ")
}

#[derive(Debug)]
pub enum PrPackMatch {
    Matched(Box<CardIllustration>, u64),
    Ambiguous(Vec<(CardIllustration, u64)>),
    NotFound,
}

/// Matches each card of a pack picture to a promo illustration, see [`pick_pr_pack_match`]:
/// first one printed in the language of the pack (`candidates`), then, for a card that matches
/// none of them, one released in the other language only (`preemptive`, see
/// [`pr_pack_preemptive_candidates`]).
pub fn match_pr_pack_image(
    img: &RgbImage,
    candidates: &[CardIllustration],
    preemptive: &[CardIllustration],
    language: Language,
    grids: &ImageGrids,
) -> Vec<PrPackMatch> {
    split_cards(img)
        .into_iter()
        .map(|(x, y, w, h)| {
            let card = image::imageops::crop_imm(img, x, y, w, h).to_image();
            // most pictures match as they are: the trimmed ones are only for the others
            let picture = CardPicture::new(&card, language);
            if let found @ PrPackMatch::Matched(..) =
                pick_pr_pack_match(candidates, &[picture], grids)
            {
                return found;
            }
            let pictures = card_pictures(&card, language);
            match pick_pr_pack_match(candidates, &pictures, grids) {
                PrPackMatch::NotFound => pick_pr_pack_match(preemptive, &pictures, grids),
                found => found,
            }
        })
        .collect()
}

/// A card cut from a pack picture: its hash, and its grid to compare its artwork.
pub struct CardPicture {
    pub hash: String,
    pub grid: Grid,
}

impl CardPicture {
    pub fn new(card: &RgbImage, language: Language) -> Self {
        Self {
            hash: to_image_hash(card, Some(language)),
            grid: Grid::of(card),
        }
    }
}

/// The closest illustration to a card picture (`pictures`, see [`card_pictures`]): within
/// [`DIST_TOLERANCE_SAME_RARITY`] and ahead of the next illustration by
/// [`PR_PACK_AMBIGUOUS_MARGIN`], or within [`PR_PACK_FAR_TOLERANCE`] and far ahead of the next
/// one ([`PR_PACK_FAR_MARGIN`]). When illustrations are about as close (cards with the same frame
/// and text box, e.g. two staff supports), the one whose artwork is far closer than the others'
/// ([`art_distance`], [`PR_PACK_ART_MARGIN`]), with the grids of their saved images.
pub fn pick_pr_pack_match(
    candidates: &[CardIllustration],
    pictures: &[CardPicture],
    grids: &ImageGrids,
) -> PrPackMatch {
    let mut dists = candidates
        .iter()
        .map(|c| {
            let dist = pictures
                .iter()
                .map(|picture| dist_hash(&c.img_hash, &picture.hash))
                .min()
                .unwrap_or(u64::MAX);
            (c, dist)
        })
        .filter(|(_, d)| *d <= PR_PACK_FAR_TOLERANCE)
        .collect_vec();
    dists.sort_by_key(|(c, d)| (*d, c.card_number.clone()));
    let Some(&(best, dist)) = dists.first() else {
        return PrPackMatch::NotFound;
    };
    let lead = dists.get(1).map_or(u64::MAX, |(_, d)| d - dist);
    if (dist <= DIST_TOLERANCE_SAME_RARITY && lead >= PR_PACK_AMBIGUOUS_MARGIN)
        || lead >= PR_PACK_FAR_MARGIN
    {
        PrPackMatch::Matched(Box::new(best.clone()), dist)
    } else if dist <= DIST_TOLERANCE_SAME_RARITY {
        // the illustrations about as close, by the distance of their artwork
        let mut arts = dists
            .iter()
            .take_while(|(_, d)| d - dist < PR_PACK_AMBIGUOUS_MARGIN)
            .map(|&(c, d)| {
                let art = grids.grid(c).map_or(f32::MAX, |illust| {
                    pictures
                        .iter()
                        .map(|picture| art_distance(&illust.grid, &picture.grid))
                        .fold(f32::MAX, f32::min)
                });
                (c, d, art)
            })
            .collect_vec();
        arts.sort_by(|a, b| a.2.total_cmp(&b.2));
        match &arts[..] {
            [(best, dist, art), (_, _, next_art), ..] if next_art - art >= PR_PACK_ART_MARGIN => {
                PrPackMatch::Matched(Box::new((*best).clone()), *dist)
            }
            _ => PrPackMatch::Ambiguous(
                arts.iter()
                    .take(2)
                    .map(|(c, d, _)| ((*c).clone(), *d))
                    .collect(),
            ),
        }
    } else {
        PrPackMatch::NotFound
    }
}

/// A card picture, as cut from a pack picture and with its edges trimmed: the
/// pictures come with drop shadows, borders and a few pixels of background around the card, that
/// the official images don't have. Each side is trimmed by 0 to [`PR_PACK_MAX_TRIM`] of the
/// card in [`PR_PACK_TRIM_STEPS`] steps, keeping the shape of a card ([`PR_PACK_TRIMMED_RATIO`]).
pub fn card_pictures(card: &RgbImage, language: Language) -> Vec<CardPicture> {
    let (width, height) = card.dimensions();
    let trims = (0..=PR_PACK_TRIM_STEPS)
        .map(|step| PR_PACK_MAX_TRIM * step as f32 / PR_PACK_TRIM_STEPS as f32)
        .collect_vec();
    let mut pictures = vec![CardPicture::new(card, language)];
    for (&left, &top, &right, &bottom) in itertools::iproduct!(&trims, &trims, &trims, &trims) {
        let x = (width as f32 * left) as u32;
        let y = (height as f32 * top) as u32;
        let w = width - x - (width as f32 * right) as u32;
        let h = height - y - (height as f32 * bottom) as u32;
        if (x, y, w, h) == (0, 0, width, height)
            || !PR_PACK_TRIMMED_RATIO.contains(&(w as f32 / h as f32))
        {
            continue;
        }
        let trimmed = image::imageops::crop_imm(card, x, y, w, h).to_image();
        pictures.push(CardPicture::new(&trimmed, language));
    }
    pictures
}

/// Background of the card pictures: white or nearly white, with little saturation. The sheet's own
/// pictures are pure white (255) and the official banners have panels like (248, 247, 252), while
/// card frames are darker or saturated.
const WHITE_MIN: u8 = 232;
const WHITE_SPREAD: u8 = 20;
/// A color (4 bits per channel) covering at least 1/25 (4%) of a picture is background (banners,
/// panels). A card covers 3-8% of a picture of 9-20 cards, spread over many colors.
const BACKGROUND_SHARE: u32 = 25;
/// Width / height of a card (63x88 mm, 0.716), with room for cropping, shadows and rounded corners.
const CARD_RATIO: std::ops::RangeInclusive<f32> = 0.62..=0.80;
/// Smaller cards are mostly upscaling for the 32x32 perceptual hash.
const CARD_MIN_HEIGHT: u32 = 80;
/// A card is at least 1/8 of the height of the picture: logos, icons and text are smaller.
const CARD_MIN_PICTURE_SHARE: u32 = 8;
/// Most of the bounding box of a card is not background (the art), unlike a frame or an outline.
const CARD_MIN_FILL: f32 = 0.5;

/// Finds the cards in a picture of several cards on a plain background: the bounding boxes
/// (x, y, width, height) of the connected non background areas that have the shape of a card,
/// in reading order. The background is first white, which keeps cards with a light frame whole,
/// then the colors covering a large part of the picture (banner backgrounds and panels).
pub fn split_cards(img: &RgbImage) -> Vec<(u32, u32, u32, u32)> {
    let near_white = |[r, g, b]: [u8; 3]| {
        let min = r.min(g).min(b);
        let max = r.max(g).max(b);
        min >= WHITE_MIN && max - min <= WHITE_SPREAD
    };
    let mut cards = card_shaped_areas(img, near_white);

    // 4 bits per channel color histogram
    let bin = |[r, g, b]: [u8; 3]| {
        ((r as usize >> 4) << 8) | ((g as usize >> 4) << 4) | (b as usize >> 4)
    };
    let mut histogram = vec![0u32; 1 << 12];
    for p in img.pixels() {
        histogram[bin(p.0)] += 1;
    }
    // large colors, and their close neighbors for shadows and gradients
    let large = (img.width() * img.height()) / BACKGROUND_SHARE;
    let mut background_bins = vec![false; 1 << 12];
    for (i, _) in histogram.iter().enumerate().filter(|(_, c)| **c >= large) {
        let (r, g, b) = ((i >> 8) as i32, ((i >> 4) & 0xF) as i32, (i & 0xF) as i32);
        for (dr, dg, db) in itertools::iproduct!(-1..=1, -1..=1, -1..=1) {
            let (r, g, b) = (r + dr, g + dg, b + db);
            if [r, g, b].iter().all(|c| (0..16).contains(c)) {
                background_bins[((r << 8) | (g << 4) | b) as usize] = true;
            }
        }
    }
    let large_colors = card_shaped_areas(img, |p| near_white(p) || background_bins[bin(p)]);

    // keep the first found, they are more complete
    let overlaps = |&(x1, y1, w1, h1): &(u32, u32, u32, u32),
                    &(x2, y2, w2, h2): &(u32, u32, u32, u32)| {
        x1 < x2 + w2 && x2 < x1 + w1 && y1 < y2 + h2 && y2 < y1 + h1
    };
    for card in large_colors {
        if !cards.iter().any(|c| overlaps(c, &card)) {
            cards.push(card);
        }
    }

    // reading order, by rows of cards
    cards.sort_by_key(|&(x, y, _, h)| ((y + h / 2) / (h.max(1)), x));
    cards
}

/// Bounding boxes of the connected non background areas that have the shape of a card.
fn card_shaped_areas(
    img: &RgbImage,
    is_background: impl Fn([u8; 3]) -> bool,
) -> Vec<(u32, u32, u32, u32)> {
    let (width, height) = img.dimensions();
    let background = img.pixels().map(|p| is_background(p.0)).collect_vec();

    let mut visited = vec![false; (width * height) as usize];
    let mut boxes = vec![];
    let mut stack = vec![];
    for start_y in 0..height {
        for start_x in 0..width {
            let start = (start_y * width + start_x) as usize;
            if visited[start] || background[start] {
                continue;
            }
            // flood fill
            visited[start] = true;
            stack.push((start_x, start_y));
            let (mut min_x, mut min_y, mut max_x, mut max_y) = (start_x, start_y, start_x, start_y);
            let mut count = 0u64;
            while let Some((x, y)) = stack.pop() {
                count += 1;
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
                let neighbors = [
                    (x.wrapping_sub(1), y),
                    (x + 1, y),
                    (x, y.wrapping_sub(1)),
                    (x, y + 1),
                ];
                for (nx, ny) in neighbors {
                    if nx >= width || ny >= height {
                        continue;
                    }
                    let n = (ny * width + nx) as usize;
                    if !visited[n] && !background[n] {
                        visited[n] = true;
                        stack.push((nx, ny));
                    }
                }
            }
            let (w, h) = (max_x - min_x + 1, max_y - min_y + 1);
            boxes.push((min_x, min_y, w, h, count));
        }
    }

    // card shaped, big enough, mostly filled
    boxes
        .into_iter()
        .filter(|&(_, _, w, h, count)| {
            CARD_RATIO.contains(&(w as f32 / h as f32))
                && h >= CARD_MIN_HEIGHT
                && h >= height / CARD_MIN_PICTURE_SHARE
                && count as f32 >= CARD_MIN_FILL * (w * h) as f32
        })
        .map(|(x, y, w, h, _)| (x, y, w, h))
        .collect()
}

#[cfg(test)]
mod pr_pack_tests {
    use super::*;

    #[test]
    fn test_promo_sources_shared_by_variants() {
        let art = FIXED_IMAGE_HASHES[0].1;
        let other_art = FIXED_IMAGE_HASHES[1].1;
        let printing =
            |jp: Option<u32>, en: Option<u32>, hash: &str, cell: Option<&str>| CardIllustration {
                card_number: "hBP01-007".into(),
                rarity: CardRarity::Promo,
                manage_id: Localized {
                    japanese: jp.map(|id| [id].into()),
                    english: en.map(|id| [id].into()),
                },
                img_hash: hash.into(),
                ogbajoj_sheet_cells: cell.map(|c| [(5, SheetCell::from(c.to_string()))].into()),
                ..Default::default()
            };
        let mut all_cards = CardsDatabase::new();
        all_cards
            .entry("hBP01-007".into())
            .or_default()
            .illustrations = vec![
            // the champion, matched to the row
            printing(Some(1445), None, art, Some("C34")),
            // top 4: same art, no row of its own
            printing(Some(1446), None, art, None),
            // the english print of the same art
            printing(None, Some(900), art, None),
            // another promo of the card, with another art
            printing(Some(1500), None, other_art, None),
        ];
        let sources = HashMap::from([(
            (5, 34, 3),
            (
                "hBP01-007",
                Some("Exstreamer Cup Finals 2025 prizes\nBloom Cup 2026 top 8 (EN)"),
            ),
        )]);

        let no_packs = PrPacks::both(BTreeMap::new(), BTreeMap::new());
        let (updated, kept, shared, moved) =
            update_promo_sources(&mut all_cards, Some((&sources, &HashSet::new())), &no_packs);
        assert_eq!((updated, kept, shared, moved), (3, 0, 2, 0));
        let promo_sources = all_cards["hBP01-007"]
            .illustrations
            .iter()
            .map(|i| i.promo_sources.clone())
            .collect_vec();
        let jp = Localized::jp(vec!["Exstreamer Cup Finals 2025 prizes".to_string()]);
        assert_eq!(promo_sources[0], jp);
        assert_eq!(promo_sources[1], jp);
        // only the line of its language
        assert_eq!(
            promo_sources[2],
            Localized::en(vec!["Bloom Cup 2026 top 8".to_string()])
        );
        assert_eq!(promo_sources[3], Localized::default());

        // a printing with a cell in an unread tab keeps its sources, and so do the variants
        // sharing its row
        let (_, kept, _, _) = update_promo_sources(
            &mut all_cards,
            Some((&sources, &HashSet::from([5]))),
            &no_packs,
        );
        assert_eq!(kept, 3);
        assert_eq!(all_cards["hBP01-007"].illustrations[0].promo_sources, jp);
        assert_eq!(all_cards["hBP01-007"].illustrations[1].promo_sources, jp);

        // the sheet changed: the cell is now the row of another card, the sources are kept
        let moved_sources =
            HashMap::from([((5, 34, 3), ("hBP01-008", Some("Another card's source")))]);
        let (_, kept, _, moved) = update_promo_sources(
            &mut all_cards,
            Some((&moved_sources, &HashSet::new())),
            &no_packs,
        );
        assert_eq!((kept, moved), (3, 1));
        assert_eq!(all_cards["hBP01-007"].illustrations[0].promo_sources, jp);
        assert_eq!(all_cards["hBP01-007"].illustrations[1].promo_sources, jp);
    }

    #[test]
    fn test_unreleased_products() {
        let sheet = |id: SheetId, title: &str| Sheet {
            properties: SheetProperties {
                sheet_id: id,
                title: title.into(),
            },
        };
        let row = |sheet_id: SheetId, row_idx: usize, number: &str, language: Language| SheetCard {
            sheet_id,
            row_idx,
            col_idx: 3,
            set_code: number.into(),
            card_name_jp_en: String::new(),
            language,
            images_src: String::new(),
            full_size_img_url: String::new(),
            hash_img_url: String::new(),
            card_type: String::new(),
            rarity: String::new(),
            color: String::new(),
            life_hp: String::new(),
            tags: String::new(),
            baton_pass: String::new(),
            text: String::new(),
            source: None,
            pack: None,
        };
        let printing =
            |number: &str, manage_id: Option<u32>, cell: (SheetId, &str)| CardIllustration {
                card_number: number.into(),
                manage_id: manage_id
                    .map(|id| Localized::jp([id].into()))
                    .unwrap_or_default(),
                ogbajoj_sheet_cells: Some([(cell.0, SheetCell::from(cell.1.to_string()))].into()),
                ..Default::default()
            };
        let mut all_cards = CardsDatabase::new();
        for illust in [
            // unreleased, in the tab of a new booster
            printing("hBP10-001", None, (1, "C3")),
            // a reprint in that booster
            printing("hBP01-032", None, (1, "C4")),
            // released: the official site has its products
            printing("hBP09-001", Some(3000), (1, "C5")),
            // an english exclusive in a start deck tab about several decks
            printing("hSD21-002", None, (2, "C3")),
            // a promo
            printing("hBP10-050", None, (3, "C3")),
            // the sheet changed: the cell is another card's row
            printing("hBP10-002", None, (1, "C6")),
            // a tab that couldn't be read
            printing("hBP10-003", None, (4, "C3")),
        ] {
            all_cards
                .entry(illust.card_number.clone())
                .or_default()
                .illustrations
                .push(illust);
        }
        all_cards.get_mut("hBP10-003").unwrap().illustrations[0].products =
            Localized::jp(vec!["hBP10".into()]);
        let sheets = [
            sheet(1, "Booster hBP10 Glory Echo"),
            sheet(2, "Start Deck hSD20-21: Kyoko & Mela"),
            sheet(3, "Promo Art 2026"),
            sheet(4, "Booster hBP11"),
            sheet(5, "Booster hBP09 Volume Vortex"),
        ];
        let tabs = vec![
            (
                &sheets[0],
                Some(vec![
                    row(1, 3, "hBP10-001", Language::Japanese),
                    row(1, 4, "hBP01-032", Language::Japanese),
                    row(1, 5, "hBP09-001", Language::Japanese),
                    row(1, 6, "hBP10-099", Language::Japanese),
                ]),
            ),
            (
                &sheets[1],
                Some(vec![row(2, 3, "hSD21-002", Language::English)]),
            ),
            (
                &sheets[2],
                Some(vec![row(3, 3, "hBP10-050", Language::Japanese)]),
            ),
            (&sheets[3], None),
            (&sheets[4], Some(vec![])),
        ];
        let mut products = ProductsDatabase::new();
        products.insert("hSD21".into(), Default::default());

        let (updated, kept) = update_unreleased_products(&mut all_cards, &mut products, &tabs);
        assert_eq!((updated, kept), (3, 2));
        let products_of = |number: &str| all_cards[number].illustrations[0].products.clone();
        assert_eq!(
            products_of("hBP10-001"),
            Localized::jp(vec!["hBP10".into()])
        );
        assert_eq!(
            products_of("hBP01-032"),
            Localized::jp(vec!["hBP10".into()])
        );
        assert_eq!(products_of("hBP09-001"), Localized::default());
        assert_eq!(
            products_of("hSD21-002"),
            Localized::en(vec!["hSD21".into()])
        );
        assert_eq!(products_of("hBP10-050"), Localized::default());
        assert_eq!(products_of("hBP10-002"), Localized::default());
        assert_eq!(
            products_of("hBP10-003"),
            Localized::jp(vec!["hBP10".into()])
        );
        // the new booster is added to the catalog, with the category of the boosters (none known
        // here)
        assert_eq!(products.keys().collect_vec(), ["hBP10", "hSD21"]);
        assert_eq!(products["hBP10"].category, None);
        products.insert(
            "hBP09".into(),
            hocg_fan_sim_assets_model::Product {
                category: Some("boosters".into()),
                ..Default::default()
            },
        );
        update_unreleased_products(&mut all_cards, &mut products, &tabs);
        assert_eq!(products["hBP10"].category.as_deref(), Some("boosters"));
    }

    #[test]
    fn test_promo_sources_with_pr_packs() {
        let printing = |id: u32, cell: Option<&str>| CardIllustration {
            card_number: "hBP01-009".into(),
            rarity: CardRarity::Promo,
            manage_id: Localized::jp([id].into()),
            ogbajoj_sheet_cells: cell.map(|c| [(5, SheetCell::from(c.to_string()))].into()),
            ..Default::default()
        };
        let mut all_cards = CardsDatabase::new();
        all_cards
            .entry("hBP01-009".into())
            .or_default()
            .illustrations = vec![printing(1, Some("C10")), printing(2, None)];
        let sources = HashMap::from([(
            (5, 10, 3),
            ("hBP01-009", Some("holodori PR Pack vol.1\nVJump 2025")),
        )]);
        let keys = all_cards["hBP01-009"]
            .illustrations
            .iter()
            .map(illust_key)
            .collect_vec();
        let packs = |packs: &[(usize, &[&str])]| -> BTreeMap<IllustKey, Vec<String>> {
            packs
                .iter()
                .map(|(i, names)| {
                    (
                        keys[*i].clone(),
                        names.iter().map(|n| n.to_string()).collect(),
                    )
                })
                .collect()
        };
        let pr_packs = PrPacks::both(
            packs(&[
                (0, &["holodori PR Pack vol 1", "Entry PR Pack vol 1"]),
                (1, &["Basic PR Pack vol 7"]),
            ]),
            BTreeMap::new(),
        );
        let promo_sources = |all_cards: &CardsDatabase| {
            all_cards["hBP01-009"]
                .illustrations
                .iter()
                .map(|i| i.promo_sources.japanese.clone().unwrap_or_default())
                .collect_vec()
        };

        // the source lines, then the packs they don't name
        update_promo_sources(&mut all_cards, Some((&sources, &HashSet::new())), &pr_packs);
        assert_eq!(
            promo_sources(&all_cards),
            [
                vec![
                    "holodori PR Pack vol.1",
                    "VJump 2025",
                    "Entry PR Pack vol 1"
                ],
                vec!["Basic PR Pack vol 7"],
            ]
        );

        // the packs couldn't be read: the stored sources are kept, with the new lines
        let sources = HashMap::from([(
            (5, 10, 3),
            ("hBP01-009", Some("VJump 2025\nholoca campaign")),
        )]);
        update_promo_sources(
            &mut all_cards,
            Some((&sources, &HashSet::new())),
            &PrPacks::default(),
        );
        assert_eq!(
            promo_sources(&all_cards),
            [
                vec![
                    "VJump 2025",
                    "holoca campaign",
                    "holodori PR Pack vol.1",
                    "Entry PR Pack vol 1"
                ],
                vec!["Basic PR Pack vol 7"],
            ]
        );

        // no source column: kept, with the packs added
        let pr_packs = PrPacks::both(packs(&[(1, &["Basic PR Pack vol 8"])]), BTreeMap::new());
        update_promo_sources(&mut all_cards, None, &pr_packs);
        assert_eq!(
            promo_sources(&all_cards)[1],
            vec!["Basic PR Pack vol 7", "Basic PR Pack vol 8"]
        );

        // both read: replaced (a pack the sheet no longer lists is dropped)
        update_promo_sources(&mut all_cards, Some((&sources, &HashSet::new())), &pr_packs);
        assert_eq!(
            promo_sources(&all_cards),
            [
                vec!["VJump 2025", "holoca campaign"],
                vec!["Basic PR Pack vol 8"]
            ]
        );
    }

    /// A tab of card rows, split in two tables like large tabs sometimes are, with the frozen rows
    /// bar after the header.
    #[test]
    fn test_spreadsheet_data_of_split_tables() {
        let sheet_id: SheetId = 77;
        let row = |r: usize, code: &str, source: &str| {
            format!(
                "<tr><th id=\"77R{r}\"><div>{}</div></th><td>{code}</td><td>name</td><td><img src=\"https://example.com/{code}=w100\"></td><td>P</td><td>{source}</td><td>{code} line (JP)</td></tr>",
                r + 1
            )
        };
        let html = format!(
            "<table><tbody><tr><th id=\"77R0\"><div>1</div></th><td>Setcode</td><td>Card Name</td><td>Image</td><td>Rarity</td><td>Source(s)</td><td>Pack</td></tr>\
             <tr><th class=\"freezebar-cell\"></th></tr>{}{}</tbody></table>\
             <table><tbody>{}</tbody></table>",
            row(1, "hBD24-001", "Birthday merch"),
            row(2, "hPR-001", "Album bonus"),
            row(3, "hBP01-021", "Basic PR Pack vol 1 (JP)"),
        );
        let cache = tempfile::tempdir().unwrap();
        let dir = cache.path().join("ogbajoj_sheet_cache");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("sheet_{sheet_id}.html")), html).unwrap();
        let sheet = Sheet {
            properties: SheetProperties {
                sheet_id,
                title: "Some tab".into(),
            },
        };

        let cards = retrieve_spreadsheet_data(&sheet, Some(cache.path())).unwrap();
        // all the rows, numbered as the sheet's rows (C2 = row 2)
        assert_eq!(
            cards
                .iter()
                .map(|c| (c.set_code.as_str(), c.row_idx, c.source.as_deref()))
                .collect_vec(),
            [
                ("hBD24-001", 2, Some("Birthday merch")),
                ("hPR-001", 3, Some("Album bonus")),
                ("hBP01-021", 4, Some("Basic PR Pack vol 1 (JP)")),
            ]
        );
        assert_eq!(cards[2].pack.as_deref(), Some("hBP01-021 line (JP)"));
    }

    const JP_SHEET_ID: SheetId = 1000451537;
    const EN_SHEET_ID: SheetId = 1030850007;
    // trimmed copies of the real tabs (2026-11-15)
    const JP_HTML: &str = include_str!("../tests/fixtures/pr_pack_contents_jp.html");
    const EN_HTML: &str = include_str!("../tests/fixtures/pr_pack_contents_en.html");
    // the Sheets API gave 21 pixels for every row of both tabs (1030 and 1001 rows)
    const JP_ROWS: usize = 1030;
    const EN_ROWS: usize = 1001;

    /// (pack, number of images) in sheet order
    fn packs(layout: &PrPackLayout) -> Vec<(String, usize)> {
        layout
            .images
            .iter()
            .chunk_by(|i| i.pack.clone())
            .into_iter()
            .map(|(pack, images)| (pack, images.count()))
            .collect()
    }

    #[test]
    fn test_jp_pr_pack_contents() {
        let layout = parse_pr_pack_sheet(JP_HTML, JP_SHEET_ID, &vec![21; JP_ROWS]);
        assert_eq!(layout.unplaced, 0);
        assert_eq!(layout.images.len(), 47);
        let expected = [
            ("Entry PR Pack vol 1", 2),
            ("Basic PR Pack vol 1", 2),
            ("Entry PR Pack vol 2", 2),
            ("Basic PR Pack vol 2", 2),
            ("Super PR Pack vol 1", 2),
            ("Entry PR Pack vol 3", 2),
            ("Basic PR Pack vol 3", 2),
            ("Basic PR Pack vol 4", 2),
            // "(cards in this pack are not foiled)" stripped
            ("Mikodanye PR Pack", 2),
            ("Basic PR Pack vol 5", 2),
            ("Basic PR Pack vol 6", 2),
            ("Super PR Pack vol 2", 1),
            ("Summer PR Pack 2025", 1),
            ("Basic PR Pack vol 7", 2),
            ("Super PR pack vol 3", 1),
            ("Basic PR Pack vol 8", 2),
            ("Basic PR Pack vol 9", 2),
            ("Basic PR Pack vol 10", 2),
            ("holodori PR Pack vol.1", 2),
            ("Basic PR Pack vol 11", 2),
            ("Super PR Pack vol 4", 1),
            ("Super PR Pack vol 5", 1),
            ("Basic PR Pack vol 12", 2),
            ("Basic PR Pack vol 13", 1),
            ("holodori PR Pack vol.2", 2),
            ("Summer PR Pack 2026", 1),
            ("Basic PR Pack vol 14", 1),
            // "(available for purchases of boxes late 2026-early 2027)" stripped
            ("Reward Cheer Pack", 1),
        ];
        assert_eq!(
            packs(&layout),
            expected.map(|(p, n)| (p.to_string(), n)).to_vec()
        );
        // the first row and the note after "Basic PR Pack vol 4" are not packs
        assert_eq!(layout.notes.len(), 2);
        assert!(layout.notes[0].starts_with("All cards in PR packs are foil"));
        assert_eq!(
            layout.notes[1],
            "(Entry PR Packs have been discontinued\nas of vol 4)"
        );
        // no image goes to a note
        for image in &layout.images {
            assert!(!image.pack.starts_with('(') && !image.pack.starts_with("All cards"));
        }
    }

    #[test]
    fn test_en_pr_pack_contents() {
        let layout = parse_pr_pack_sheet(EN_HTML, EN_SHEET_ID, &vec![21; EN_ROWS]);
        assert_eq!(layout.unplaced, 0);
        assert_eq!(layout.images.len(), 27);
        let expected = [
            ("Welcome Pack 2025", 2),
            ("Monthly Tournament Pack Vol.01 2025", 2),
            // the image starts 1 pixel above the heading row
            ("Monthly Tournament Pack Vol. 02 2025?", 1),
            ("Monthly Tournament Pack Vol. 03 2025", 2),
            ("Monthly Tournament Pack Vol.01 2026", 2),
            ("Monthly Tournament Pack Vol.02 2026", 2),
            ("Advent/Justice PR Pack vol 1", 1),
            // one image is positioned from the first row: posObj(.., 0, 0, 0, 5397)
            ("Monthly Tournament Pack Vol. 3 2026", 2),
            ("Monthly Tournament Pack Vol. 4 2026", 2),
            ("Monthly Tournament Pack Vol. 5 2026", 2),
            ("Monthly Tournament Pack Vol. 6 2026", 2),
            ("holodori PR Pack vol 1", 2),
            ("Super PR Pack vol 1", 3),
            ("Monthly Tournament Pack Vol. 1 2027", 2),
        ];
        assert_eq!(
            packs(&layout),
            expected.map(|(p, n)| (p.to_string(), n)).to_vec()
        );
        assert!(layout.notes.is_empty());
    }

    #[test]
    fn test_pr_pack_contents_other_sheet_or_missing_rows() {
        // the positions of another tab are ignored
        let layout = parse_pr_pack_sheet(JP_HTML, EN_SHEET_ID, &vec![21; JP_ROWS]);
        assert!(layout.images.is_empty());
        // rows the API doesn't know: can't be placed
        let layout = parse_pr_pack_sheet(JP_HTML, JP_SHEET_ID, &vec![21; 100]);
        assert!(layout.unplaced > 0);
    }

    /// A sheet with text rows and images: (row, text) and (id, row, y, height)
    fn sheet(texts: &[(usize, &str)], images: &[(&str, usize, i64, i64)]) -> String {
        let rows = texts
            .iter()
            .map(|(row, text)| {
                format!(
                    "<tr><th id=\"9R{row}\"><div>{}</div></th><td>{text}</td></tr>",
                    row + 1
                )
            })
            .join("\n");
        let overlays = images
            .iter()
            .map(|(id, _, _, height)| {
                format!(
                    "<div id='{id}' class='waffle-embedded-object-overlay' style='width: 300px; height: {height}px; display: block;'><img src='https://example.com/{id}' style='display: block;'></div>"
                )
            })
            .join("\n");
        let calls = images
            .iter()
            .map(|(id, row, y, _)| format!("posObj('9', '{id}', {row}, 2, 0, {y});"))
            .join("");
        format!(
            "<table><tbody>{rows}</tbody></table>{overlays}<script>function posObj(sheet, id, row, col, x, y) {{}} function posObjs() {{ {calls} }}</script>"
        )
    }

    #[test]
    fn test_notes_never_get_images() {
        let html = sheet(
            &[
                (0, "All cards in PR packs are foil - read this first"),
                (1, "Pack A"),
                // a note between a pack and its images
                (2, "(the cards of this pack are not foiled<br>as of vol 2)"),
                (20, "Pack B (with a note)"),
                (40, "(discontinued)"),
                (50, "Pack C"),
            ],
            &[
                ("a1", 3, 0, 200),
                ("a2", 4, 0, 200),
                // starts 2 pixels above the heading row
                ("b1", 20, -2, 300),
                // a big offset from the first row
                ("c1", 0, 51 * 21, 200),
            ],
        );
        let layout = parse_pr_pack_sheet(&html, 9, &vec![21; 100]);
        assert_eq!(layout.unplaced, 0);
        assert_eq!(
            layout
                .images
                .iter()
                .map(|i| (i.pack.as_str(), &i.url[20..]))
                .collect_vec(),
            [
                ("Pack A", "a1"),
                ("Pack A", "a2"),
                ("Pack B", "b1"),
                ("Pack C", "c1")
            ]
        );
        assert_eq!(
            layout.notes,
            [
                "All cards in PR packs are foil - read this first",
                "(the cards of this pack are not foiled\nas of vol 2)",
                "(discontinued)"
            ]
        );
    }

    #[test]
    fn test_images_are_placed_with_the_row_heights() {
        // the same pixel offset lands in different rows with taller rows
        let html = sheet(&[(0, "Pack A"), (5, "Pack B")], &[("x", 0, 300, 20)]);
        let layout = parse_pr_pack_sheet(&html, 9, &[21; 20]);
        assert_eq!(layout.images[0].pack, "Pack B");
        let mut heights = vec![21; 20];
        heights[1] = 400;
        let layout = parse_pr_pack_sheet(&html, 9, &heights);
        assert_eq!(layout.images[0].pack, "Pack A");
        // hidden rows don't take space
        heights[1] = 0;
        let layout = parse_pr_pack_sheet(&html, 9, &heights);
        assert_eq!(layout.images[0].pack, "Pack B");
    }

    #[test]
    fn test_images_above_the_first_pack_are_ignored() {
        let html = sheet(&[(10, "Pack A")], &[("x", 0, 0, 20), ("y", 11, 0, 20)]);
        let layout = parse_pr_pack_sheet(&html, 9, &[21; 20]);
        assert_eq!(layout.unplaced, 0);
        assert_eq!(layout.images.len(), 1);
        assert_eq!(layout.images[0].pack, "Pack A");
    }

    #[test]
    fn test_row_heights_from_json() {
        let json = r#"{"sheets":[{"data":[{"rowMetadata":[{"pixelSize":21},{"pixelSize":219},{"pixelSize":21,"hiddenByUser":true},{"pixelSize":21,"hiddenByFilter":true},{"pixelSize":21}]}]}]}"#;
        assert_eq!(row_heights_from_json(json), Some(vec![21, 219, 0, 0, 21]));
        assert_eq!(row_heights_from_json(r#"{"sheets":[]}"#), None);
        assert_eq!(row_heights_from_json("not json"), None);
    }

    #[test]
    fn test_pr_pack_tab_language() {
        assert_eq!(
            pr_pack_tab_language("JP PR Pack contents"),
            Some(Some(Language::Japanese))
        );
        assert_eq!(
            pr_pack_tab_language("EN PR Pack contents"),
            Some(Some(Language::English))
        );
        assert_eq!(
            pr_pack_tab_language("JP PR Pack contents 2027"),
            Some(Some(Language::Japanese))
        );
        assert_eq!(
            pr_pack_tab_language("pr pack contents (EN)"),
            Some(Some(Language::English))
        );
        // no language, or both
        assert_eq!(pr_pack_tab_language("PR Pack contents"), Some(None));
        assert_eq!(pr_pack_tab_language("JP/EN PR Pack contents"), Some(None));
        // "JPN" is not a JP word, "OPEN" doesn't contain an EN word
        assert_eq!(pr_pack_tab_language("Open PR Pack contents"), Some(None));
        // not a PR pack tab
        assert_eq!(pr_pack_tab_language("PR/Birthday Cards"), None);
        assert_eq!(pr_pack_tab_language("Booster hBP10 Glory Echo"), None);
    }

    #[test]
    fn test_strip_trailing_notes() {
        assert_eq!(
            strip_trailing_notes("Mikodanye PR Pack (cards in this pack are not foiled)"),
            "Mikodanye PR Pack"
        );
        assert_eq!(
            strip_trailing_notes("(Entry PR Packs have been discontinued\nas of vol 4)"),
            ""
        );
        assert_eq!(
            strip_trailing_notes("Pack (a (nested) note) (another)"),
            "Pack"
        );
        // only trailing notes
        assert_eq!(strip_trailing_notes("Pack (JP) vol 1"), "Pack (JP) vol 1");
        assert_eq!(strip_trailing_notes("Pack  vol\n2"), "Pack vol 2");
        // unbalanced
        assert_eq!(strip_trailing_notes("Pack vol 1)"), "Pack vol 1)");
    }

    /// A picture of `cards` (x, y, color) of 63x88 cards on a background.
    fn picture(background: [u8; 3], cards: &[(u32, u32, [u8; 3])]) -> RgbImage {
        let mut img = RgbImage::from_pixel(800, 500, image::Rgb(background));
        for &(x, y, color) in cards {
            for dy in 0..176 {
                for dx in 0..126 {
                    // a darker border and some art inside
                    let c = if dx < 3 || dy < 3 || dx >= 123 || dy >= 173 {
                        [40, 40, 40]
                    } else {
                        [
                            color[0].wrapping_add((dx / 8) as u8 * 3),
                            color[1].wrapping_add((dy / 8) as u8 * 3),
                            color[2],
                        ]
                    };
                    img.put_pixel(x + dx, y + dy, image::Rgb(c));
                }
            }
        }
        img
    }

    /// A card-like picture: art, a frame and a text box, with colors from `seed`.
    fn card_art(seed: u32, width: u32, height: u32) -> RgbImage {
        RgbImage::from_fn(width, height, |x, y| {
            let (u, v) = (x * 400 / width, y * 559 / height);
            if !(12..388).contains(&u) || !(12..547).contains(&v) {
                image::Rgb([40, 40, 60])
            } else if v > 330 {
                image::Rgb([230, 225, 210])
            } else {
                let r = ((u * seed / 7 + v * 3) % 256) as u8;
                let g = ((v * seed / 5 + u) % 256) as u8;
                let b = (((u + v) * seed / 11) % 256) as u8;
                image::Rgb([r, g, b])
            }
        })
    }

    /// The saved images of test illustrations, where their grids are made from.
    struct TestImages {
        dir: tempfile::TempDir,
        count: std::cell::Cell<u32>,
        grids: ImageGrids,
    }

    impl TestImages {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let grids = ImageGrids::new(dir.path(), dir.path());
            Self {
                dir,
                count: Default::default(),
                grids,
            }
        }

        /// A promo illustration, with `img` saved as its Japanese image.
        fn promo_with(
            &self,
            number: &str,
            jp: Option<u32>,
            en: Option<u32>,
            img: &RgbImage,
        ) -> CardIllustration {
            self.count.set(self.count.get() + 1);
            let name = format!("{number}_{}.png", self.count.get());
            img.save(self.dir.path().join(&name)).unwrap();
            CardIllustration {
                card_number: number.into(),
                rarity: CardRarity::Promo,
                manage_id: Localized {
                    japanese: jp.map(|id| [id].into()),
                    english: en.map(|id| [id].into()),
                },
                img_path: Localized::jp(name),
                img_hash: to_image_hash(img, Some(Language::Japanese)),
                ..Default::default()
            }
        }

        fn promo(
            &self,
            number: &str,
            jp: Option<u32>,
            en: Option<u32>,
            seed: u32,
        ) -> CardIllustration {
            self.promo_with(number, jp, en, &card_art(seed, 400, 559))
        }
    }

    #[test]
    fn test_card_pictures_trim_shadows() {
        let images = TestImages::new();
        let illust = images.promo("hBP01-001", Some(1), None, 3);
        // a small picture of the card, with a drop shadow on the right and the bottom
        let (width, height) = (150, 210);
        let shadow = 9;
        let picture = RgbImage::from_fn(width + shadow, height + shadow, |x, y| {
            if x < width && y < height {
                *card_art(3, width, height).get_pixel(x, y)
            } else {
                image::Rgb([150, 150, 165])
            }
        });
        let pictures = card_pictures(&picture, Language::Japanese);
        assert!(pictures.len() > 1);
        let untrimmed = dist_hash(&illust.img_hash, &pictures[0].hash);
        let trimmed = pictures
            .iter()
            .map(|p| dist_hash(&illust.img_hash, &p.hash))
            .min()
            .unwrap();
        assert!(trimmed < untrimmed, "{trimmed} < {untrimmed}");
        assert!(trimmed <= DIST_TOLERANCE_SAME_RARITY, "{trimmed}");
    }

    #[test]
    fn test_pick_pr_pack_match() {
        let images = TestImages::new();
        let grids = &images.grids;
        let candidates = [
            images.promo("hBP01-001", Some(1), None, 3),
            images.promo("hBP01-002", Some(2), None, 17),
        ];
        let picture = |seed| {
            vec![CardPicture::new(
                &card_art(seed, 150, 210),
                Language::Japanese,
            )]
        };
        match pick_pr_pack_match(&candidates, &picture(3), grids) {
            PrPackMatch::Matched(illust, _) => assert_eq!(illust.card_number, "hBP01-001"),
            other => panic!("{other:?}"),
        }
        // the same art twice: can't tell them apart
        let twins = [
            candidates[0].clone(),
            images.promo("hBP01-003", Some(3), None, 3),
        ];
        assert!(matches!(
            pick_pr_pack_match(&twins, &picture(3), grids),
            PrPackMatch::Ambiguous(_)
        ));
        assert!(matches!(
            pick_pr_pack_match(&[], &picture(3), grids),
            PrPackMatch::NotFound
        ));

        // the same frame and text box, another art in the middle of the card (where the
        // perceptual hash is blurred): told apart by the artwork
        let (red, blue) = ([200, 60, 60], [60, 60, 200]);
        let with_art = |width, height, left: [u8; 3]| {
            let right = if left == red { blue } else { red };
            let mut img = card_art(3, width, height);
            for (x, y, pixel) in img.enumerate_pixels_mut() {
                if (height * 30 / 100..height * 55 / 100).contains(&y) {
                    *pixel = image::Rgb(if x < width / 2 { left } else { right });
                }
            }
            img
        };
        let staff = [
            images.promo_with("hSD01-017", Some(505), None, &with_art(400, 559, red)),
            images.promo_with("hSD14-010", Some(2470), None, &with_art(400, 559, blue)),
        ];
        let picture = vec![CardPicture::new(
            &with_art(150, 210, red),
            Language::Japanese,
        )];
        let dists = staff
            .iter()
            .map(|c| dist_hash(&c.img_hash, &picture[0].hash))
            .collect_vec();
        assert!(dists[1] - dists[0] < PR_PACK_AMBIGUOUS_MARGIN, "{dists:?}");
        match pick_pr_pack_match(&staff, &picture, grids) {
            PrPackMatch::Matched(illust, _) => assert_eq!(illust.card_number, "hSD01-017"),
            other => panic!("{other:?}"),
        }
        // without their images, the artworks can't be compared
        let missing = ImageGrids::new(Path::new("missing"), Path::new("missing"));
        assert!(matches!(
            pick_pr_pack_match(&staff, &picture, &missing),
            PrPackMatch::Ambiguous(_)
        ));
    }

    #[test]
    fn test_match_pr_pack_image_preemptive() {
        let images = TestImages::new();
        // a picture of a pack: a card on white
        let mut picture = RgbImage::from_pixel(600, 400, image::Rgb([255, 255, 255]));
        image::imageops::overlay(&mut picture, &card_art(3, 200, 280), 50, 50);
        let released = images.promo("hBP01-001", Some(1), Some(10), 3);
        let jp_only = images.promo("hBP01-001", Some(1), None, 3);
        let other = images.promo("hBP01-002", Some(2), Some(20), 17);

        let matched = |candidates: &[CardIllustration], preemptive: &[CardIllustration]| {
            match_pr_pack_image(
                &picture,
                candidates,
                preemptive,
                Language::English,
                &images.grids,
            )
            .into_iter()
            .map(|m| match m {
                PrPackMatch::Matched(illust, _) => Some(illust.manage_id.clone()),
                _ => None,
            })
            .collect_vec()
        };
        // printed in english
        assert_eq!(
            matched(
                &[released.clone(), other.clone()],
                std::slice::from_ref(&jp_only)
            ),
            [Some(released.manage_id.clone())]
        );
        // not in english yet: the japanese promo
        assert_eq!(
            matched(std::slice::from_ref(&other), std::slice::from_ref(&jp_only)),
            [Some(jp_only.manage_id.clone())]
        );
        assert_eq!(matched(&[other], &[]), [None]);
    }

    #[test]
    fn test_split_cards_on_white() {
        let img = picture(
            [255, 255, 255],
            &[
                (10, 10, [200, 30, 30]),
                (150, 10, [30, 200, 30]),
                (10, 200, [30, 30, 200]),
            ],
        );
        assert_eq!(
            split_cards(&img),
            [(10, 10, 126, 176), (150, 10, 126, 176), (10, 200, 126, 176)]
        );
    }

    #[test]
    fn test_split_cards_on_a_colored_banner() {
        // light blue banner, like the official announcements
        let img = picture(
            [150, 200, 240],
            &[(100, 100, [200, 30, 30]), (300, 100, [230, 200, 60])],
        );
        assert_eq!(
            split_cards(&img),
            [(100, 100, 126, 176), (300, 100, 126, 176)]
        );
    }

    #[test]
    fn test_split_cards_ignores_other_shapes() {
        let mut img = picture([255, 255, 255], &[(10, 10, [200, 30, 30])]);
        // a wide title and a small logo
        for y in 300..360 {
            for x in 200..700 {
                img.put_pixel(x, y, image::Rgb([20, 20, 120]));
            }
        }
        for y in 400..440 {
            for x in 600..630 {
                img.put_pixel(x, y, image::Rgb([20, 120, 20]));
            }
        }
        assert_eq!(split_cards(&img), [(10, 10, 126, 176)]);
    }
}
