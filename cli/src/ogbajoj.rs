use std::{collections::HashMap, error::Error, fs, ops::Not, path::Path, sync::Arc};

use csv::ReaderBuilder;
use hocg_fan_sim_assets_model::{
    Art, BloomLevel, Card, CardIllustration, CardType, CardsDatabase, Color, Extra, Keyword,
    KeywordEffect, Language, Localized, OshiSkill, QnaDatabase, SupportType, Tag,
};
use image::DynamicImage;
use itertools::Itertools;
use parking_lot::RwLock;
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use reqwest::header::REFERER;
use scraper::{Html, Node, Selector};
use serde::Deserialize;
use webp::{Encoder, WebPMemory};

use crate::{
    DEBUG,
    data::{update_arts, update_extra, update_keywords, update_oshi_skills, update_tags},
    http_client,
    images::{
        UNRELEASED_FOLDER, WEBP_QUALITY,
        utils::{DIST_TOLERANCE_DIFF_RARITY, DIST_TOLERANCE_SAME_RARITY, dist_hash, to_image_hash},
    },
    utils::TrimOnce,
    utils::sanitize_filename,
};

const SPREADSHEET_ID: &str = "1IdaueY-Jw8JXjYLOhA9hUd2w0VRBao9Z1URJwmCWJ64";

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
    pub row_idx: usize,
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
    pub text: String,
}

impl SheetCard {
    fn update_card(&self, card: &mut Card) {
        let card_number = card.card_number.clone();
        let released = card.illustrations.iter().any(|i| i.manage_id.has_value());

        // don't overwrite if it already exists
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
        let name = self.name();
        if card.name.japanese.is_none() || !released {
            card.name.japanese = name.japanese.as_ref().map(|n| n.replace("\n", " "));
        } else {
            // warn if the name is different (ignore names that are split)
            if card.name.japanese != name.japanese
                && name.japanese.as_ref().is_some_and(|n| !n.contains("\n"))
            {
                eprintln!(
                    "Warning: {card_number} name mismatch: {:?} should be {:?}",
                    name.japanese, card.name.japanese
                );
            }
        }
        card.name.english = name.english.clone();
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
        self.update_card_text(card, released);
        // there is no japanese text in the sheet
        // update existing tags (tags consistency check)
        update_tags(card, self.tags(), Language::English, released);
        // there is no baton pass in the sheet
        // there is no max amount in the sheet, add default max amount
        if card.max_amount.japanese.unwrap_or_default() == 0 || !released {
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

    fn update_card_text(&self, card: &mut Card, released: bool) {
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
        let oshi_skills_lines = extract_sections(&mut text_lines, &["Oshi Skill", "SP Oshi Skill"]);
        let oshi_skills = oshi_skills_lines
            .into_iter()
            .filter_map(|lines| self.to_oshi_skill(card, lines))
            .collect_vec();
        // dbg!(&oshi_skills);
        update_oshi_skills(card, oshi_skills, Language::English, released);

        // Keywords
        let keywords_lines =
            extract_sections(&mut text_lines, &["Collab Effect", "Bloom Effect", "Gift"]);
        let keywords = keywords_lines
            .into_iter()
            .filter_map(|lines| self.to_keyword(card, lines))
            .collect_vec();
        update_keywords(card, keywords, Language::English, released);

        // Arts
        let arts_lines = extract_sections(&mut text_lines, &["Arts"]);
        let arts = arts_lines
            .into_iter()
            .filter_map(|lines| self.to_art(card, lines))
            .collect_vec();
        update_arts(card, arts, Language::English, released);

        // Extra
        let extra_lines = extract_sections(&mut text_lines, &["Extra"]);
        let mut extra = extra_lines
            .into_iter()
            .filter_map(|lines| self.to_extra(card, lines))
            .next();
        fix_extra(card, &mut extra);
        update_extra(card, extra, Language::English, released);

        // Ability text
        card.ability_text.english = text_lines
            .is_empty()
            .not()
            .then(|| text_lines.join("\n").trim().to_string());
    }

    fn to_oshi_skill(&self, card: &mut Card, mut lines: Vec<&str>) -> Option<OshiSkill> {
        let first = lines.remove(0);
        let Some((mut holo_power, name)) = first.split_once(':') else {
            eprintln!("Oshi skill not found for {}", card.card_number);
            return None;
        };

        let mut special = false;
        if holo_power.starts_with("Oshi Skill") {
            special = false;
            holo_power = holo_power.trim_start_once("Oshi Skill").trim();
        } else if holo_power.starts_with("SP Oshi Skill") {
            special = true;
            holo_power = holo_power.trim_start_once("SP Oshi Skill").trim();
        }
        holo_power = holo_power.trim_start_once("holo Power").trim();
        holo_power = holo_power.trim_start_once("-");

        let name = name
            .trim()
            .trim_start_once(r#"""#)
            .trim_end_once(r#"""#)
            .trim();

        Some(OshiSkill {
            special,
            holo_power: holo_power.to_string().try_into().unwrap_or_default(),
            name: Localized::en(name.into()),
            ability_text: Localized::en(lines.join("\n").trim().to_string()),
        })
    }

    fn to_keyword(&self, card: &mut Card, mut lines: Vec<&str>) -> Option<Keyword> {
        let first = lines.remove(0);
        let Some((effect, name)) = first.split_once(':') else {
            eprintln!("Keyword not found for {}", card.card_number);
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

        let name = name
            .trim()
            .trim_start_once(r#"""#)
            .trim_end_once(r#"""#)
            .trim();

        Some(Keyword {
            effect,
            name: Localized::en(name.into()),
            ability_text: Localized::en(lines.join("\n").trim().to_string()),
        })
    }

    fn to_art(&self, card: &mut Card, mut lines: Vec<&str>) -> Option<Art> {
        let first = lines.remove(0);
        let Some((_arts, name)) = first.split_once(':') else {
            eprintln!("Art not found for {}", card.card_number);
            return None;
        };
        let name = name
            .trim()
            .trim_start_once(r#"""#)
            .trim_end_once(r#"""#)
            .trim();

        let second = lines.remove(0);
        let Some((_cost, cheers)) = second.split_once(':') else {
            eprintln!("Art cost not found for {}", card.card_number);
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

        let third = lines.remove(0);
        let Some((_power, power)) = third.split_once(':') else {
            eprintln!("Art power not found for {}", card.card_number);
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
                .then(|| Localized::en(lines.join("\n").trim().to_string())),
        })
    }

    fn to_extra(&self, card: &mut Card, mut lines: Vec<&str>) -> Option<Extra> {
        let first = lines.remove(0);
        let Some((_extra, extra)) = first.split_once(':') else {
            eprintln!("Extra not found for {}", card.card_number);
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
fn fix_extra(card: &Card, extra: &mut Option<Extra>) {
    // hSD09-003 Houshou Marine does have extra text
    if card.card_number == "hSD09-003" {
        *extra = Some(Localized::en(
            "If this holomem is downed, you get Life-2".into(),
        ));
    }

    // hBP05-029 Juufuutei Raden does have extra text
    if card.card_number == "hBP05-029" {
        *extra = Some(Localized::en(
            "If this holomem is downed, you get Life-2".into(),
        ));
    }

    // hBP05-039 Ichijou Ririka does have extra text
    if card.card_number == "hBP05-039" {
        *extra = Some(Localized::en(
            "If this holomem is downed, you get Life-2".into(),
        ));
    }

    // hSD10-010 Isaki Riona does have extra text
    if card.card_number == "hSD10-010" {
        *extra = Some(Localized::en("This holomem cannot Bloom".into()));
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
pub fn retrieve_card_info_from_ogbajoj_sheet(all_cards: &mut CardsDatabase) {
    println!("Retrieve all cards info from @ogbajoj's sheet");

    let spreadsheet = retrieve_spreadsheet();

    let mut updated_count = 0;
    let mut cheers_names = HashMap::new();

    for sheet in spreadsheet.sheets {
        let cards = retrieve_spreadsheet_data(&sheet).unwrap_or_default();

        for result in cards {
            let record: SheetCard = result;
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

            // keep track of cheer names. cheers all have the same name, but are not all in the sheet
            if card.card_type == CardType::Cheer
                && card.card_number.split_once('-').unwrap().1 == "001"
            {
                cheers_names
                    .entry(card.card_number.split_once('-').unwrap().0.to_string())
                    .or_insert(card.name.clone());
            }
        }
    }

    println!("Updated {updated_count} cards");

    let missing_english = all_cards
        .values_mut()
        // ignore and update cheer names
        .filter_map(|c| {
            if c.card_type == CardType::Cheer {
                // use the basic cheer name from the sheet
                c.name = cheers_names
                    .get(c.card_number.split_once('-').unwrap().0)
                    .cloned()?;
                None
            } else {
                Some(c)
            }
        })
        .filter(|c| c.name.english.is_none())
        .count();
    println!("Missing english names: {missing_english}");
}

pub fn download_images_from_ogbajoj_sheet(
    images_jp_path: &Path,
    images_en_path: &Path,
    all_cards: &mut CardsDatabase,
) {
    println!("Downloading unreleased images from @ogbajoj's sheet...");

    let all_cards = Arc::new(RwLock::new(all_cards));

    let spreadsheet = retrieve_spreadsheet();

    // this is used to delay Master Sheet access
    let master_sheet_lock = Arc::new(RwLock::new(()));

    // Iterate html files inside the ZIP
    spreadsheet
        .sheets
        .into_par_iter()
        .filter_map(move |sheet| {
            let name = sheet.properties.title.clone();

            // Skip Master Sheet. Low quality images, wrong data, and duplicates (2025-09)
            if name == "Master Sheet" {
                return None;
            }

            let cards = retrieve_spreadsheet_data(&sheet);

            Some((name, cards?))
        })
        .for_each({
            let all_cards = all_cards.clone();
            let master_sheet_lock = master_sheet_lock.clone();
            move |(name, cards)| {
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
                    let set_code = card.set_code;
                    let rarity = card.rarity;
                    let img_src = card.images_src;
                    let hash_img_url = card.hash_img_url;
                    let full_size_img_url = card.full_size_img_url;

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
                    let img_hash = to_image_hash(&hash_img.into_rgb8());

                    // skip rejected hashes
                    if is_skip_image_hash(&img_hash) {
                        if DEBUG {
                            println!(
                                "[{name}] row {row_idx} {set_code}: skipped image hash {img_hash:?}"
                            );
                        }
                        skipped += 1;
                        continue;
                    }

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
                            .filter(|i| {
                                i.card_number == set_code
                                    && (i.rarity.eq_ignore_ascii_case(&rarity)
                                        || !i.manage_id.has_value())
                            })
                            .map(|i| (dist_hash(&i.img_hash, &img_hash), i))
                            // more tolerance for manual cropping
                            .filter(|(dist, i)| {
                                match (
                                    i.manage_id.has_value(),                // released
                                    i.rarity.eq_ignore_ascii_case(&rarity), // same rarity
                                ) {
                                    (true, true) => *dist <= DIST_TOLERANCE_SAME_RARITY,
                                    (true, false) => unreachable!(),
                                    (false, true) => {
                                        rarity != "P" || *dist <= DIST_TOLERANCE_SAME_RARITY
                                    }
                                    (false, false) => *dist <= DIST_TOLERANCE_DIFF_RARITY,
                                }
                            })
                            .collect_vec();
                        matching_illustrations.sort_by_key(|(dist, _)| *dist);

                        let illust = if let Some((_, illust)) = matching_illustrations.first_mut() {
                            // Use existing unreleased illustration, with a different image
                            // Master Sheet has duplicate entries
                            if !illust.manage_id.has_value()
                                && illust.img_hash != img_hash
                                && name != "Master Sheet"
                            {
                                _adding = false;

                                // if the rarity is different, rename the new file if needed
                                if let Some(img_path) = illust.img_path.value(language)
                                    && illust.rarity == rarity
                                {
                                    img_unreleased = Path::new(img_path).into();
                                } else {
                                    illust.rarity = rarity.clone();
                                    *illust.img_path.value_mut(language) =
                                        Some(img_unreleased.to_str().unwrap().replace("\\", "/"))
                                }

                                illust
                            } else {
                                // it's an unreleased card that might have an updated rarity
                                if !illust.manage_id.has_value() && illust.rarity != rarity {
                                    illust.rarity = rarity.clone();
                                    // rename the old file
                                    if let Some(img_path) =
                                        illust.img_path.value_mut(language).as_mut()
                                    {
                                        let old_path = images_path.join(&img_path);
                                        *img_path =
                                            img_unreleased.to_str().unwrap().replace("\\", "/");
                                        let new_path = images_path.join(&img_path);
                                        fs::rename(old_path, new_path).unwrap();
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
                                rarity: rarity.clone(),
                                img_path: Localized::new(
                                    language,
                                    img_unreleased.to_str().unwrap().replace("\\", "/"),
                                ),
                                ..Default::default()
                            });
                            card.illustrations.last_mut().unwrap()
                        };

                        // could be cleared on errors, with path
                        illust.img_hash = img_hash.clone();
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
                            .find(|i| i.rarity == rarity && i.img_hash == img_hash)
                            .unwrap();
                        // could not download image
                        illust.img_hash = Default::default();
                        *illust.img_path.value_mut(language) = None;
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

fn is_skip_image_hash(img_hash: &str) -> bool {
    let to_skip = [
        // hBP05-079 (P) Miko, I'm ashamed - poor quality
        "v2|H32=Uq2rEtSqKHWrYtRzqkxrnfVaVK0rZ5XivFIrrWES1IhqFZLtVKosmqpULZqqV62KcnirZJJFmyxGk2pVcLhSlY6HUlU|CYM=0C0F1C",
    ];

    to_skip.contains(&img_hash)
}

pub fn retrieve_qna_from_ogbajoj_sheet(all_qnas: &mut QnaDatabase) {
    println!("Retrieve all Q&As info from @ogbajoj's sheet");

    let api_key = std::env::var("GOOGLE_SHEETS_API_KEY").expect("GOOGLE_SHEETS_API_KEY not set");

    let spreadsheet = retrieve_spreadsheet();

    let sheets_gid = spreadsheet
        .sheets
        .iter()
        .filter(|s| s.properties.title.contains("Q&A"))
        .map(|s| s.properties.sheet_id)
        .collect_vec();
    // dbg!(&sheets_gid);

    let mut updated_count = 0;

    let url = format!("https://docs.google.com/spreadsheets/d/{SPREADSHEET_ID}/export");
    for gid in sheets_gid {
        let resp = http_client()
            .get(&url)
            .query(&[
                ("id", SPREADSHEET_ID),
                ("gid", gid.to_string().as_str()),
                ("format", "csv"),
                ("key", api_key.as_str()), // probably doesn't do anything
            ])
            .send()
            .unwrap();
        let content = resp.text().unwrap();
        // fs::write(format!("sheet_{gid}.csv"), &content).unwrap();

        let mut rdr = ReaderBuilder::new()
            .has_headers(false)
            .from_reader(content.as_bytes());
        for (question, answer) in rdr
            .records()
            .flatten()
            .filter(|r| !r.is_empty())
            .tuple_windows()
        {
            if !question[0].starts_with('Q') || !answer[0].starts_with('A') {
                continue;
            }

            let Some((number, question)) = question[0].split_once('-') else {
                eprintln!("Invalid question format: {}", &question[0]);
                continue;
            };
            let Some((_, answer)) = answer[0].split_once('-') else {
                eprintln!("Invalid answer format: {}", &answer[0]);
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

fn retrieve_spreadsheet() -> Spreadsheet {
    let api_key = std::env::var("GOOGLE_SHEETS_API_KEY").expect("GOOGLE_SHEETS_API_KEY not set");

    let url = format!("https://sheets.googleapis.com/v4/spreadsheets/{SPREADSHEET_ID}");
    let resp = http_client()
        .get(url)
        .query(&[("key", api_key.as_str())])
        .send()
        .unwrap();

    let content = resp.text().unwrap();
    let spreadsheet: Spreadsheet = serde_json::from_str(&content).unwrap();
    // dbg!(&spreadsheet);

    spreadsheet
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

fn retrieve_spreadsheet_data(sheet: &Sheet) -> Option<Vec<SheetCard>> {
    // Precompute selectors
    let table_sel = Selector::parse("table").unwrap();
    let tr_sel = Selector::parse("tr").unwrap();
    let td_sel = Selector::parse("td").unwrap();
    let img_sel = Selector::parse("img").unwrap();

    let name = sheet.properties.title.clone();

    // Read HTML content from the website
    let url = format!("https://docs.google.com/spreadsheets/d/{SPREADSHEET_ID}/htmlembed");
    let resp = http_client()
        .get(&url)
        .query(&[
            ("gid", sheet.properties.sheet_id.to_string().as_str()),
            ("widget", "false"),
            ("single", "true"),
        ])
        .header("Sec-Fetch-Dest", "document")
        .header("Sec-Fetch-Mode", "navigate")
        .header("Sec-Fetch-Site", "none")
        .header("Sec-Fetch-User", "?1")
        .send()
        .unwrap();
    let html = resp.text().unwrap();

    println!("[{name}] Reading HTML...");

    let document = Html::parse_document(&html);
    if let Some(table) = document.select(&table_sel).next() {
        // Build header index map from the first tbody row of <td> labels
        let trs = table.select(&tr_sel).collect_vec();
        let mut set_code_idx: Option<usize> = None;
        let mut card_name_idx: Option<usize> = None;
        let mut image_idx: Option<usize> = None;
        let mut card_type_idx: Option<usize> = None;
        let mut rarity_1_idx: Option<usize> = None;
        let mut color_idx: Option<usize> = None;
        let mut life_hp_idx: Option<usize> = None;
        let mut tags_idx: Option<usize> = None;
        let mut text_idx = None;
        let mut alternate_art_idx: Option<usize> = None;
        let mut rarity_2_idx: Option<usize> = None;
        let mut alternate_art_2_idx: Option<usize> = None;
        let mut rarity_3_idx: Option<usize> = None;
        let mut source_idx = None;
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
            alternate_art_idx = None;
            rarity_2_idx = None;
            alternate_art_2_idx = None;
            rarity_3_idx = None;
            text_idx = None;
            source_idx = None;

            let headers = header_tr
                .select(&td_sel)
                .map(|td| td.text().collect::<String>().trim().to_ascii_lowercase())
                .collect_vec();
            for (idx, h) in headers.iter().enumerate() {
                if h.contains("setcode") || h.contains("set code") {
                    set_code_idx = Some(idx);
                }

                if h.contains("card name") {
                    card_name_idx = Some(idx);
                }

                if h.contains("image") {
                    image_idx = Some(idx);
                }

                if h.contains("type") {
                    card_type_idx = Some(idx);
                }

                if h.contains("color") {
                    color_idx = Some(idx);
                }

                if h.contains("life") || h.contains("hp") {
                    life_hp_idx = Some(idx);
                }

                if h.contains("tags") {
                    tags_idx = Some(idx);
                }

                if h.contains("alternate art") {
                    if h.contains("2") {
                        alternate_art_2_idx = Some(idx);
                    } else {
                        alternate_art_idx = Some(idx);
                    }
                }

                if h.contains("rarity") {
                    if rarity_1_idx.is_none() {
                        // first rarity column
                        rarity_1_idx = Some(idx);
                    } else if rarity_2_idx.is_none() {
                        // second rarity column
                        rarity_2_idx = Some(idx);
                    } else if rarity_3_idx.is_none() {
                        // third rarity column
                        rarity_3_idx = Some(idx);
                    }
                }

                if h.contains("text") {
                    text_idx = Some(idx);
                }

                if h.contains("source") {
                    source_idx = Some(idx);
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
        for (row_idx, tr) in trs.into_iter().enumerate().skip(header_row_idx + 1) {
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
            let text = extract_cell_text(&tds, text_idx);
            let source = extract_cell_text(&tds, source_idx);

            // language switch for English exclusive cards like hY01-008
            let language = if source.contains("(JP)") {
                Language::Japanese
            } else if text.contains("EN exclusive")
                || source.contains("EN only")
                || source.contains("(EN)")
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
                images.push((img_src, rarity));
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
                images.push((alt_art_src, rarity));
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
                images.push((alt_art_src, rarity));
            }

            for (img_src, rarity) in images {
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
                    row_idx,
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
                    text: text.clone(),
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

// Image URLs are external; fetch via HTTP
fn download_sheet_image(
    url: &str,
    row_idx: usize,
    set_code: &str,
    name: &str,
) -> Result<DynamicImage, Box<dyn Error>> {
    let resp = http_client()
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
