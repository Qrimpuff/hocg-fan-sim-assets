pub mod decklog {

    use std::{fmt::Display, str::FromStr, sync::Arc};

    use hocg_fan_sim_assets_model::{
        BloomLevel, CardIllustration, CardType, CardsDatabase, Localized, SupportType,
    };
    use parking_lot::{Mutex, RwLock};
    use rayon::iter::{IntoParallelIterator, ParallelBridge, ParallelIterator};
    use reqwest::header::REFERER;
    use serde::{Deserialize, Deserializer, Serialize};

    use crate::http_client;

    #[derive(Debug, Serialize)]
    #[serde(rename_all = "snake_case")]
    struct ApiSearchRequest {
        page: u32,
        param: ApiSearchParam,
    }
    #[derive(Debug, Serialize)]
    #[serde(rename_all = "snake_case")]
    struct ApiSearchParam {
        deck_param1: String,
        deck_type: String,
        keyword: String,
        keyword_type: Vec<String>,
        expansion: String,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub struct DeckLogCard {
        #[serde(deserialize_with = "deserialize_nullable_number_from_string")]
        pub manage_id: Option<u32>,
        pub card_number: String,
        pub card_kind: String,
        pub name: String,
        pub rare: String,
        pub img: String,
        pub bloom_level: String,
        #[serde(deserialize_with = "deserialize_number_from_string")]
        pub max: u32,
    }

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrInt<T> {
        String(String),
        Number(T),
    }

    fn deserialize_number_from_string<'de, T, D>(deserializer: D) -> Result<T, D::Error>
    where
        D: Deserializer<'de>,
        T: FromStr + serde::Deserialize<'de>,
        <T as FromStr>::Err: Display,
    {
        match StringOrInt::<T>::deserialize(deserializer)? {
            StringOrInt::String(s) => s.parse::<T>().map_err(serde::de::Error::custom),
            StringOrInt::Number(i) => Ok(i),
        }
    }

    fn deserialize_nullable_number_from_string<'de, T, D>(
        deserializer: D,
    ) -> Result<Option<T>, D::Error>
    where
        D: Deserializer<'de>,
        T: FromStr + serde::Deserialize<'de>,
        <T as FromStr>::Err: Display,
    {
        match Option::<StringOrInt<T>>::deserialize(deserializer)? {
            Some(StringOrInt::String(s)) => s
                .parse::<T>()
                .map_err(serde::de::Error::custom)
                .map(Option::Some),
            Some(StringOrInt::Number(i)) => Ok(Some(i)),
            None => Ok(None),
        }
    }

    impl DeckLogCard {
        fn card_type(&self) -> CardType {
            match self.card_kind.trim().to_lowercase().as_str() {
                s if s.contains("推し") => CardType::OshiHoloMember,
                s if s.contains("ホロメン") => CardType::HoloMember,
                s if s.contains("スタッフ") => CardType::Support(SupportType::Staff),
                s if s.contains("アイテム") => CardType::Support(SupportType::Item),
                s if s.contains("イベント") => CardType::Support(SupportType::Event),
                s if s.contains("ツール") => CardType::Support(SupportType::Tool),
                s if s.contains("マスコット") => CardType::Support(SupportType::Mascot),
                s if s.contains("ファン") => CardType::Support(SupportType::Fan),
                "エール" => CardType::Cheer,
                _ => CardType::Other,
            }
        }

        fn bloom_level(&self) -> Option<BloomLevel> {
            let card_type = self.bloom_level.trim().to_lowercase();
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
            self.card_kind.trim().to_lowercase().contains("buzz")
        }

        fn limited(&self) -> bool {
            self.card_kind.trim().to_lowercase().contains("limited")
        }
    }

    // Retrieve the following fields from decklog:
    // - Card number
    // - Manage ID (unique id)
    // - Rarity
    // - Image URL
    // - Max amount
    // - Deck type (oshi, cheer, etc)
    pub fn retrieve_card_info_from_decklog(
        all_cards: &mut CardsDatabase,
        number_filter: &Option<String>,
        expansion: &Option<String>,
        optimized_original_images: bool,
    ) -> Vec<(String, usize)> {
        if number_filter.is_none() && expansion.is_none() {
            println!("Retrieve ALL cards info from Deck Log");
        } else {
            println!(
                "Retrieve cards info from Deck Log with filters - number: {}, expension: {}",
                number_filter.as_deref().unwrap_or("all"),
                expansion.as_deref().unwrap_or("all")
            );
        }

        let filtered_cards = Arc::new(Mutex::new(Vec::new()));
        let all_cards = Arc::new(RwLock::new(all_cards));

        let _ = ["N", "OSHI", "YELL"]
            .into_par_iter()
            .flat_map({
                let filtered_cards = filtered_cards.clone();
                let all_cards = all_cards.clone();
                move |deck_type| {
                    (1..)
                        .par_bridge()
                        .map({
                            let filtered_cards = filtered_cards.clone();
                            let all_cards = all_cards.clone();
                            move |page| {
                                println!("deck type: {deck_type}, page: {page}");

                                let req = ApiSearchRequest {
                                    param: ApiSearchParam {
                                        deck_param1: "S".into(),
                                        deck_type: deck_type.into(),
                                        keyword: number_filter.clone().unwrap_or_default(),
                                        keyword_type: vec!["no".into()],
                                        expansion: expansion.clone().unwrap_or_default(),
                                    },
                                    page,
                                };

                                let resp = http_client()
                                    .post("https://decklog.bushiroad.com/system/app/api/search/9")
                                    .header(REFERER, "https://decklog.bushiroad.com/")
                                    .json(&req)
                                    .send()
                                    .unwrap();

                                let content = resp.text().unwrap();
                                // println!("{content}");
                                let cards = serde_json::from_str(&content);
                                let Ok(cards): Result<Vec<DeckLogCard>, _> = cards else {
                                    eprintln!("didn't like response: {content}");
                                    panic!("{:?}", cards)
                                };

                                // no more card in this page
                                if cards.is_empty() {
                                    return None;
                                }

                                // update records with deck type and webp images
                                for mut dl_card in cards {
                                    if !optimized_original_images {
                                        dl_card.img = dl_card.img.replace(".png", ".webp");
                                    }

                                    // remove the old manage_id if it exists
                                    all_cards
                                        .write()
                                        .values_mut()
                                        .flat_map(|cs| cs.illustrations.iter_mut())
                                        .filter(|c| {
                                            c.manage_id == dl_card.manage_id
                                                && c.card_number != dl_card.card_number
                                        })
                                        .for_each(|c| c.manage_id = None);

                                    // add the card the list
                                    let mut all_cards = all_cards.write();
                                    let card =
                                        all_cards.entry(dl_card.card_number.clone()).or_default();
                                    card.card_number = dl_card.card_number.clone();
                                    card.name.japanese = Some(dl_card.name.clone());
                                    card.card_type = dl_card.card_type();
                                    if let CardType::OshiHoloMember = card.card_type {
                                        card.max_amount = dl_card.max.min(1)
                                    } else {
                                        card.max_amount = dl_card.max
                                    }
                                    card.bloom_level = dl_card.bloom_level();
                                    card.buzz = dl_card.buzz();
                                    card.limited = dl_card.limited();

                                    let illustrations = &mut card.illustrations;
                                    // find the card, first by manage_id, then by image, then overwrite delta, otherwise just add
                                    if let Some(illust) = {
                                        if let Some(i) = illustrations
                                            .iter_mut()
                                            .find(|i| i.manage_id == dl_card.manage_id)
                                        {
                                            Some(i)
                                        } else if let Some(i) = illustrations.iter_mut().find(|i| {
                                            i.img_path.japanese.as_ref() == Some(&dl_card.img)
                                        }) {
                                            Some(i)
                                        } else {
                                            illustrations.iter_mut().find(|c| c.manage_id.is_none())
                                        }
                                    } {
                                        // only these fields are retrieved
                                        illust.card_number = dl_card.card_number;
                                        illust.manage_id = dl_card.manage_id;
                                        illust.rarity = dl_card.rare;
                                        illust.img_path.japanese = Some(dl_card.img);
                                    } else {
                                        let illust = CardIllustration {
                                            card_number: dl_card.card_number.clone(),
                                            manage_id: dl_card.manage_id,
                                            rarity: dl_card.rare,
                                            img_path: Localized::jp(dl_card.img),
                                            ..Default::default()
                                        };
                                        // add the card to the list
                                        illustrations.push(illust);
                                    }

                                    // sort the list, by oldest to latest
                                    illustrations.sort_by_key(|c| c.manage_id);

                                    // add to filtered cards
                                    filtered_cards.lock().push(dl_card.manage_id);
                                }

                                Some(())
                            }
                        })
                        .while_some()
                }
            })
            .max(); // need this to drive the iterator

        let all_cards = all_cards.read();
        let filtered_cards = filtered_cards.lock();
        all_cards
            .values()
            .flat_map(|cs| cs.illustrations.iter().enumerate())
            .filter(|c| filtered_cards.contains(&c.1.manage_id))
            .map(|c| (c.1.card_number.clone(), c.0))
            .collect()
    }
}

pub mod ogbajoj {

    use std::{collections::HashMap, ops::Not};

    use hocg_fan_sim_assets_model::{
        Art, BloomLevel, Card, CardType, CardsDatabase, Color, Extra, Keyword, KeywordEffect,
        Localized, OshiSkill, SupportType,
    };
    use itertools::Itertools;
    use serde::{Deserialize, Serialize};

    use crate::http_client;

    #[derive(Deserialize, Debug)]
    #[serde(rename_all = "camelCase")]
    pub struct Spreadsheet {
        pub properties: SpreadsheetProperties,
        pub sheets: Vec<Sheet>,
    }

    #[derive(Deserialize, Debug)]
    #[serde(rename_all = "camelCase")]
    pub struct SpreadsheetProperties {
        pub title: String,
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
        // pub title: String,
    }
    #[derive(Debug, Deserialize, Serialize)]
    pub struct SheetCard {
        #[serde(rename = "Setcode")]
        pub set_code: String,
        #[serde(rename = "Card Name \"JP (EN)\"")]
        pub card_name_jp_en: String,
        #[serde(rename = "Type")]
        pub card_type: String,
        #[serde(rename = "Color")]
        pub color: String,
        #[serde(rename = "LIFE/HP")]
        pub life_hp: String,
        #[serde(rename = "Tags")]
        pub tags: String,
        #[serde(rename = "Text")]
        pub text: String,
    }

    impl SheetCard {
        fn update_card(&self, card: &mut Card) {
            let card_number = card.card_number.clone();

            // don't overwrite if it already exists
            if card.card_number.is_empty() {
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
            if card.name.japanese.is_none() {
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
            if card.card_type == Default::default() {
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
            if card.colors.is_empty() {
                card.colors = self.colors();
            } else {
                // warn if the colors are different
                let mut colors_1 = self.colors().clone();
                colors_1.sort();
                let mut colors_2 = card.colors.clone();
                colors_2.sort();
                if colors_1 != colors_2 {
                    eprintln!(
                        "Warning: {card_number} colors mismatch: {:?} should be {:?}",
                        colors_1, colors_2
                    );
                }
            }
            if card.card_type == CardType::OshiHoloMember {
                if card.life == 0 {
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
                if card.hp == 0 {
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
            if card.bloom_level == Default::default() {
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
            self.update_card_text(card);
            // there is no japanese text in the sheet
            // update existing tags (tags consistency check)
            card.tags.iter_mut().zip(self.tags()).for_each(|(t, s)| {
                t.english = Some(s);
            });
            // there is no baton pass in the sheet
            // there is no max amount in the sheet
        }

        fn update_card_text(&self, card: &mut Card) {
            let text = self.text();
            let mut text_lines = text.lines().map(|l| l.trim()).collect_vec();

            fn extract_sections<'a>(
                lines: &mut Vec<&'a str>,
                starts: &[&str],
            ) -> Vec<Vec<&'a str>> {
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
            let oshi_skills_lines =
                extract_sections(&mut text_lines, &["Oshi Skill", "SP Oshi Skill"]);
            let oshi_skills = oshi_skills_lines
                .into_iter()
                .filter_map(|lines| self.to_oshi_skill(card, lines))
                .collect_vec();
            // dbg!(&oshi_skills);
            self.update_oshi_skills(card, oshi_skills);

            // Keywords
            let keywords_lines =
                extract_sections(&mut text_lines, &["Collab Effect", "Bloom Effect", "Gift"]);
            let keywords = keywords_lines
                .into_iter()
                .filter_map(|lines| self.to_keyword(card, lines))
                .collect_vec();
            self.update_keywords(card, keywords);

            // Arts
            let arts_lines = extract_sections(&mut text_lines, &["Arts"]);
            let arts = arts_lines
                .into_iter()
                .filter_map(|lines| self.to_art(card, lines))
                .collect_vec();
            self.update_arts(card, arts);

            // Extra
            let extra_lines = extract_sections(&mut text_lines, &["Extra"]);
            let extra = extra_lines
                .into_iter()
                .filter_map(|lines| self.to_extra(card, lines))
                .next();
            self.update_extra(card, extra);

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
                holo_power = holo_power.trim_start_matches("Oshi Skill").trim();
            } else if holo_power.starts_with("SP Oshi Skill") {
                special = true;
                holo_power = holo_power.trim_start_matches("SP Oshi Skill").trim();
            }
            holo_power = holo_power.trim_start_matches("holo Power").trim();
            holo_power = holo_power.trim_start_matches('-');

            let name = name
                .trim()
                .trim_start_matches('"')
                .trim_end_matches('"')
                .trim();

            Some(OshiSkill {
                special,
                holo_power: holo_power.to_string().try_into().unwrap_or_default(),
                name: Localized::en(name.into()),
                ability_text: Localized::en(lines.join("\n").trim().to_string()),
            })
        }

        fn update_oshi_skills(&self, card: &mut Card, oshi_skills: Vec<OshiSkill>) {
            // Apply English text to original card oshi_skills, matching by holo_power if possible
            for (orig_skill, sheet_skill) in card.oshi_skills.iter_mut().zip(oshi_skills.iter()) {
                if orig_skill.special != sheet_skill.special {
                    eprintln!(
                        "Warning: {} oshi skill special mismatch: {} should be {}",
                        card.card_number, sheet_skill.special, orig_skill.special
                    );
                }
                if orig_skill.holo_power != sheet_skill.holo_power {
                    eprintln!(
                        "Warning: {} oshi skill holo power mismatch: {} should be {}",
                        card.card_number,
                        String::from(sheet_skill.holo_power),
                        String::from(orig_skill.holo_power)
                    );
                }
                orig_skill.name.english = sheet_skill.name.english.clone();
                orig_skill.ability_text.english = sheet_skill.ability_text.english.clone();
            }
            // If the number of sheet oshi_skills is different from the original, warn
            if oshi_skills.len() != card.oshi_skills.len() {
                eprintln!(
                    "Warning: {} has a different number of oshi skills in the sheet than in the original card: {} should be {}",
                    card.card_number,
                    oshi_skills.len(),
                    card.oshi_skills.len()
                );
            }
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
                    eprintln!("Unknown keyword effect: {}", effect);
                    return None;
                }
            };

            let name = name
                .trim()
                .trim_start_matches('"')
                .trim_end_matches('"')
                .trim();

            Some(Keyword {
                effect,
                name: Localized::en(name.into()),
                ability_text: Localized::en(lines.join("\n").trim().to_string()),
            })
        }

        fn update_keywords(&self, card: &mut Card, keywords: Vec<Keyword>) {
            // Apply English text to original card keywords, matching by effect if possible
            for (orig_keyword, sheet_keyword) in card.keywords.iter_mut().zip(keywords.iter()) {
                if orig_keyword.effect != sheet_keyword.effect {
                    eprintln!(
                        "Warning: {} keyword effect mismatch: {:?} should be {:?}",
                        card.card_number, sheet_keyword.effect, orig_keyword.effect
                    );
                }
                orig_keyword.name.english = sheet_keyword.name.english.clone();
                orig_keyword.ability_text.english = sheet_keyword.ability_text.english.clone();
            }
            // If the number of sheet keywords is different from the original, warn
            if keywords.len() != card.keywords.len() {
                eprintln!(
                    "Warning: {} has a different number of keywords in the sheet than in the original card: {} should be {}",
                    card.card_number,
                    keywords.len(),
                    card.keywords.len()
                );
            }
        }

        fn to_art(&self, card: &mut Card, mut lines: Vec<&str>) -> Option<Art> {
            let first = lines.remove(0);
            let Some((_arts, name)) = first.split_once(':') else {
                eprintln!("Art not found for {}", card.card_number);
                return None;
            };
            let name = name
                .trim()
                .trim_start_matches('"')
                .trim_end_matches('"')
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
                            .trim_start_matches('+')
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

        fn update_arts(&self, card: &mut Card, arts: Vec<Art>) {
            // Apply English text to original card arts, matching by power if possible
            for (orig_art, sheet_art) in card.arts.iter_mut().zip(arts.iter()) {
                if orig_art.cheers != sheet_art.cheers {
                    eprintln!(
                        "Warning: {} art cheers mismatch: {:?} should be {:?}",
                        card.card_number, sheet_art.cheers, orig_art.cheers
                    );
                }
                if orig_art.power != sheet_art.power {
                    eprintln!(
                        "Warning: {} art power mismatch: {:?} should be {:?}",
                        card.card_number, sheet_art.power, orig_art.power
                    );
                }
                if orig_art.advantage != sheet_art.advantage {
                    eprintln!(
                        "Warning: {} art advantage mismatch: {:?} should be {:?}",
                        card.card_number, sheet_art.advantage, orig_art.advantage
                    );
                }
                orig_art.name.english = sheet_art.name.english.clone();

                if let Some(orig_ability_text) = &mut orig_art.ability_text {
                    if let Some(sheet_ability_text) = &sheet_art.ability_text {
                        orig_ability_text.english = sheet_ability_text.english.clone();
                    } else {
                        eprintln!(
                            "Warning: {} art ability text mismatch: {:?} should be {:?}",
                            card.card_number, sheet_art.ability_text, orig_ability_text
                        );
                    }
                } else if sheet_art.ability_text.is_some() {
                    eprintln!(
                        "Warning: {} art ability text mismatch: {:?} should be {:?}",
                        card.card_number, sheet_art.ability_text, orig_art.ability_text
                    );
                }
            }
            // If the number of sheet arts is different from the original, warn
            if arts.len() != card.arts.len() {
                eprintln!(
                    "Warning: {} has a different number of arts in the sheet than in the original card: {} should be {}",
                    card.card_number,
                    arts.len(),
                    card.arts.len()
                );
            }
        }

        fn to_extra(&self, card: &mut Card, mut lines: Vec<&str>) -> Option<Extra> {
            let first = lines.remove(0);
            let Some((_extra, extra)) = first.split_once(':') else {
                eprintln!("Extra not found for {}", card.card_number);
                return None;
            };

            Some(Localized::new("".into(), extra.trim().into()))
        }

        fn update_extra(&self, card: &mut Card, extra: Option<Extra>) {
            // Apply English text to original card extra, matching by name if possible
            for (orig_extra, sheet_extra) in card.extra.iter_mut().zip(extra.iter()) {
                orig_extra.english = sheet_extra.english.clone();
            }
            // If the number of sheet extra is different from the original, warn
            if extra.is_some() != card.extra.is_some() {
                eprintln!(
                    "Warning: {} has a different extra in the sheet than in the original card: {} should be {}",
                    card.card_number,
                    extra.is_some(),
                    card.extra.is_some()
                );
            }
        }

        fn name(&self) -> Localized<String> {
            if let Some((jp, en)) = self.card_name_jp_en.split_once("\n(") {
                Localized::new(
                    jp.trim().into(),
                    en.trim_start_matches('(')
                        .trim_end_matches(')')
                        .trim()
                        .into(),
                )
            } else {
                let name = self.card_name_jp_en.trim();
                Localized::new(name.into(), name.into())
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

        fn tags(&self) -> Vec<String> {
            self.tags
                .split_whitespace()
                .map(|s| s.trim().to_string())
                .collect()
        }
    }

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

        const SPREADSHEET_ID: &str = "1IdaueY-Jw8JXjYLOhA9hUd2w0VRBao9Z1URJwmCWJ64";

        let api_key =
            std::env::var("GOOGLE_SHEETS_API_KEY").expect("GOOGLE_SHEETS_API_KEY not set");

        let url = format!("https://sheets.googleapis.com/v4/spreadsheets/{SPREADSHEET_ID}");
        let resp = http_client()
            .get(url)
            .query(&[("key", api_key.as_str())])
            .send()
            .unwrap();

        let content = resp.text().unwrap();
        let spreadsheet: Spreadsheet = serde_json::from_str(&content).unwrap();
        // dbg!(&spreadsheet);

        if spreadsheet.properties.title != "Hololive OCG/TCG card translations" {
            eprintln!("Wrong spreadsheet");
            return;
        }

        let sheets_gid = spreadsheet
            .sheets
            .iter()
            .map(|s| s.properties.sheet_id)
            .collect_vec();
        // dbg!(&sheets_gid);

        let mut updated_count = 0;
        let mut cheers_names = HashMap::new();

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

            let mut rdr = csv::Reader::from_reader(content.as_bytes());
            for result in rdr.deserialize().flatten() {
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
                if card.card_type == CardType::Cheer {
                    cheers_names.insert(
                        card.card_number.split_once('-').unwrap().0.to_string(),
                        card.name.english.clone(),
                    );
                }
            }
        }

        println!("Updated {updated_count} cards");

        let missing_english = all_cards
            .values_mut()
            // ignore and update cheer names
            .filter_map(|c| {
                if c.card_type == CardType::Cheer {
                    // use the cheer name from the sheet
                    c.name.english = cheers_names
                        .get(c.card_number.split_once('-').unwrap().0)
                        .cloned()
                        .flatten();
                    None
                } else {
                    Some(c)
                }
            })
            .filter(|c| c.name.english.is_none())
            .count();
        println!("Missing english names: {missing_english}");

        // check for tags consistency
        let tags_mapping = all_cards
            .values()
            .flat_map(|c| &c.tags)
            .map(|t| (&t.japanese, &t.english))
            .unique()
            .into_group_map_by(|t| t.0);
        for (tag, names) in tags_mapping {
            if names.len() > 1 {
                println!("Tag {tag:?} has multiple names: {names:#?}");
            }
        }
    }
}

pub mod hololive_official {
    use std::sync::Arc;
    use std::{ops::Deref, sync::OnceLock};

    use hocg_fan_sim_assets_model::{
        Art, BloomLevel, Card, CardType, CardsDatabase, Color, Keyword, KeywordEffect, Localized,
        OshiSkill,
    };
    use itertools::Itertools;
    use parking_lot::{Mutex, RwLock};
    use rayon::iter::{ParallelBridge, ParallelIterator};
    use reqwest::Url;
    use scraper::{Html, Node, Selector};

    use crate::http_client;

    fn card_type_from_str(card_type: &str) -> CardType {
        match card_type.trim().to_lowercase().as_str() {
            s if s.contains("推し") => CardType::OshiHoloMember,
            s if s.contains("ホロメン") => CardType::HoloMember,
            s if s.contains("スタッフ") => {
                CardType::Support(hocg_fan_sim_assets_model::SupportType::Staff)
            }
            s if s.contains("アイテム") => {
                CardType::Support(hocg_fan_sim_assets_model::SupportType::Item)
            }
            s if s.contains("イベント") => {
                CardType::Support(hocg_fan_sim_assets_model::SupportType::Event)
            }
            s if s.contains("ツール") => {
                CardType::Support(hocg_fan_sim_assets_model::SupportType::Tool)
            }
            s if s.contains("マスコット") => {
                CardType::Support(hocg_fan_sim_assets_model::SupportType::Mascot)
            }
            s if s.contains("ファン") => {
                CardType::Support(hocg_fan_sim_assets_model::SupportType::Fan)
            }
            "エール" => CardType::Cheer,
            _ => CardType::Other,
        }
    }

    fn buzz(card_type: &str) -> bool {
        card_type.trim().to_lowercase().contains("buzz")
    }

    fn limited(card_type: &str) -> bool {
        card_type.trim().to_lowercase().contains("limited")
    }

    fn tags_from_str(tags: &str) -> Vec<Localized<String>> {
        tags.split_whitespace()
            .map(|s| s.trim().to_string())
            .map(Localized::jp)
            .collect()
    }

    fn colors_from_str(colors: &str) -> Vec<Color> {
        colors
            .chars()
            .filter(|c| !c.is_whitespace())
            .flat_map(|s| match s {
                '白' => Some(Color::White),
                '緑' => Some(Color::Green),
                '赤' => Some(Color::Red),
                '青' => Some(Color::Blue),
                '紫' => Some(Color::Purple),
                '黄' => Some(Color::Yellow),
                '◇' => Some(Color::Colorless),
                _ => None,
            })
            .collect()
    }

    fn bloom_level_from_str(bloom_level: &str) -> Option<BloomLevel> {
        let bloom_level = bloom_level.trim().to_lowercase();
        match bloom_level.as_str() {
            s if s.contains("1st") => Some(BloomLevel::First),
            s if s.contains("2nd") => Some(BloomLevel::Second),
            s if s.contains("debut") => Some(BloomLevel::Debut),
            s if s.contains("spot") => Some(BloomLevel::Spot),
            _ => None,
        }
    }

    fn baton_pass_from_str(baton_pass: &str) -> Vec<Color> {
        colors_from_str(baton_pass)
    }

    fn update_card_info(card: &mut Card, hololive_card: &scraper::ElementRef) {
        static CARD_NAME: OnceLock<Selector> = OnceLock::new();
        let card_name = CARD_NAME.get_or_init(|| Selector::parse(".name").unwrap());

        // Card name
        let card_name = hololive_card
            .select(card_name)
            .next()
            .map(|c| c.text().collect::<String>());
        // from Deck Log
        if card.name.japanese.is_none() {
            card.name.japanese = card_name;
        }

        static INFO: OnceLock<Selector> = OnceLock::new();
        let info = INFO.get_or_init(|| Selector::parse(".info dl :is(dt, dd)").unwrap());

        // Card info
        for (key, value) in hololive_card.select(info).tuples() {
            let key = key.text().collect::<String>();
            let img_alts = value
                .children()
                .filter_map(|c| c.value().as_element().and_then(|c| c.attr("alt")))
                .join(" ");
            let value = value.text().collect::<String>();
            let value = if !img_alts.is_empty() {
                img_alts.trim()
            } else {
                value.trim()
            };

            match key.to_lowercase().as_str() {
                "カードタイプ" => {
                    // from Deck Log
                    if card.card_type == Default::default() {
                        card.card_type = card_type_from_str(value);
                        card.buzz = buzz(value);
                        card.limited = limited(value);
                    } else {
                        // warn if the type is different
                        let card_type = card_type_from_str(value);
                        if card.card_type != card_type {
                            eprintln!(
                                "Warning: {} type mismatch: {:?} should be {:?}",
                                card.card_number, card_type, card.card_type
                            );
                        }
                        let buzz = buzz(value);
                        if card.buzz != buzz {
                            eprintln!(
                                "Warning: {} buzz mismatch: {} should be {}",
                                card.card_number, buzz, card.buzz
                            );
                        }
                        let limited = limited(value);
                        if card.limited != limited {
                            eprintln!(
                                "Warning: {} limited mismatch: {} should be {}",
                                card.card_number, limited, card.limited
                            );
                        }
                    }
                }
                "タグ" => card.tags = tags_from_str(value), // replace existing tags. will need to import english tags later
                "色" => card.colors = colors_from_str(value),
                "life" => card.life = value.parse().unwrap_or_default(),
                "hp" => card.hp = value.parse().unwrap_or_default(),
                "bloomレベル" => {
                    // from Deck Log
                    if card.bloom_level == Default::default() {
                        card.bloom_level = bloom_level_from_str(value);
                    } else {
                        // warn if the level is different
                        let bloom_level = bloom_level_from_str(value);
                        if card.bloom_level != bloom_level {
                            eprintln!(
                                "Warning: {} bloom level mismatch: {:?} should be {:?}",
                                card.card_number, bloom_level, card.bloom_level
                            );
                        }
                    }
                }
                "バトンタッチ" => card.baton_pass = baton_pass_from_str(value),
                "能力テキスト" => {
                    card.ability_text.japanese = Some(
                        value
                            .trim_end_matches("LIMITED：ターンに１枚しか使えない。") // remove the limited line
                            .trim()
                            .to_string(),
                    )
                }
                _ => { /* nothing else */ }
            }
        }

        static EXTRA: OnceLock<Selector> = OnceLock::new();
        let extra = EXTRA.get_or_init(|| Selector::parse(".extra p:nth-child(2)").unwrap());

        // Extra
        // replace existing extras. will need to import english extras later
        card.extra = hololive_card
            .select(extra)
            .next()
            .map(|c| Localized::jp(c.text().collect::<String>()));
    }

    fn update_card_oshi_skills(card: &mut Card, hololive_card: &scraper::ElementRef) {
        static OSHI_SKILL: OnceLock<Selector> = OnceLock::new();
        let oshi_skill =
            OSHI_SKILL.get_or_init(|| Selector::parse(".oshi.skill p:nth-child(2)").unwrap());
        static SP_SKILL: OnceLock<Selector> = OnceLock::new();
        let sp_skill =
            SP_SKILL.get_or_init(|| Selector::parse(".sp.skill p:nth-child(2)").unwrap());

        let card_number = &card.card_number;

        let mut oshi_skills = vec![];
        for (special, oshi_skill) in hololive_card
            .select(oshi_skill)
            .map(|s| (false, s))
            .chain(hololive_card.select(sp_skill).map(|s| (true, s)))
        {
            let Some((holo_power, name, text)) = oshi_skill.text().collect_tuple() else {
                eprintln!("Oshi skill not found for {card_number:?}");
                continue;
            };

            let oshi_skill = OshiSkill {
                special,
                holo_power: holo_power
                    .trim_start_matches("[ホロパワー：")
                    .trim_start_matches("-")
                    .trim_end_matches("]")
                    .trim_end_matches("消費")
                    .to_string()
                    .try_into()
                    .unwrap_or_default(),
                name: Localized::jp(name.trim().to_string()),
                ability_text: Localized::jp(text.trim().to_string()),
            };
            oshi_skills.push(oshi_skill);
        }
        // replace existing keywords. will need to import english keywords later
        card.oshi_skills = oshi_skills;
    }

    fn update_card_keywords(card: &mut Card, hololive_card: &scraper::ElementRef) {
        static KEYWORD: OnceLock<Selector> = OnceLock::new();
        let keyword = KEYWORD.get_or_init(|| Selector::parse(".keyword p:nth-child(2)").unwrap());

        let card_number = &card.card_number;
        // Keywords
        let mut keywords = vec![];
        for keyword in hololive_card.select(keyword) {
            let mut keyword_text = String::new();
            for node in keyword.descendants() {
                // add card text
                if let Node::Text(text) = node.value() {
                    keyword_text.push_str(text.deref());
                    keyword_text.push('\n');

                // include alt text for images e.g. cheers, color advantages, etc
                } else if let Node::Element(el) = node.value() {
                    if let Some(alt) = el.attr("alt") {
                        keyword_text.push_str(alt.to_string().as_str());
                        keyword_text.push('\n');
                    }
                }
            }

            // dbg!(&keyword_text);

            let mut keyword_text = keyword_text
                .lines()
                .map(|l| l.trim())
                .filter(|l| !l.is_empty());
            let Some(effect) = keyword_text.next() else {
                eprintln!("Keyword effect not found for {card_number:?}");
                continue;
            };
            let Some(name) = keyword_text.next() else {
                eprintln!("Keyword name not found for {card_number:?}");
                continue;
            };
            let text = keyword_text.collect_vec().join("\n");
            if text.trim().is_empty() {
                eprintln!("Keyword text not found for {card_number:?}");
                continue;
            };

            let keyword = Keyword {
                effect: match effect.trim() {
                    "コラボエフェクト" => KeywordEffect::Collab,
                    "ブルームエフェクト" => KeywordEffect::Bloom,
                    "ギフト" => KeywordEffect::Gift,
                    _ => KeywordEffect::Other,
                },
                name: Localized::jp(name.trim().to_string()),
                ability_text: Localized::jp(text.trim().to_string()),
            };
            keywords.push(keyword);
        }
        // replace existing keywords. will need to import english keywords later
        card.keywords = keywords;
    }

    fn update_card_arts(card: &mut Card, hololive_card: &scraper::ElementRef) {
        static ART: OnceLock<Selector> = OnceLock::new();
        let art = ART.get_or_init(|| Selector::parse(".sp.arts p:nth-child(2)").unwrap());

        let card_number = &card.card_number;

        // Arts
        let mut arts = vec![];
        for art in hololive_card.select(art) {
            let mut art_text = String::new();
            for node in art.descendants() {
                // add card text
                if let Node::Text(text) = node.value() {
                    art_text.push_str(text.deref());
                    art_text.push('\n');

                // include alt text for images e.g. cheers, color advantages, etc
                } else if let Node::Element(el) = node.value() {
                    if let Some(alt) = el.attr("alt") {
                        art_text.push_str(alt.to_string().as_str());
                        art_text.push('\n');
                    }
                }
            }

            // dbg!(&art_text);

            let mut cheers = vec![];
            let mut art_text = art_text.lines().filter_map(|l| {
                let l = l.trim();
                let c = colors_from_str(l);
                if c.is_empty() || l.chars().count() > 1 {
                    Some(l)
                } else {
                    cheers.extend(c);
                    None
                }
            });

            let Some(name) = art_text.next() else {
                eprintln!("Art name not found for {card_number:?}");
                continue;
            };
            let advantage = art_text
                .next()
                .and_then(|a| a.split_once('+'))
                .map(|(a, p)| {
                    art_text.next();
                    (colors_from_str(a)[0], p.parse().unwrap_or_default())
                });
            let text = art_text.filter(|l| !l.is_empty()).collect_vec().join("\n");
            let text = if text.trim().is_empty() {
                None
            } else {
                Some(text)
            };

            let (name, power) = name.rsplit_once('　').unwrap_or((name, ""));

            let art = Art {
                cheers,
                name: Localized::jp(name.trim().to_string()),
                power: power.to_string().try_into().unwrap_or_default(),
                advantage,
                ability_text: text.map(|text| Localized::jp(text.trim().to_string())),
            };
            arts.push(art);
        }
        // replace existing arts. will need to import english arts later
        card.arts = arts;
    }

    fn update_card_illustrations(card: &mut Card, hololive_card: &scraper::ElementRef, url: &str) {
        static ILLUSTRATOR: OnceLock<Selector> = OnceLock::new();
        let illustrator = ILLUSTRATOR.get_or_init(|| Selector::parse(".ill-name span").unwrap());

        let card_number = &card.card_number;

        // need to go to the detailed page
        let Some(card_url) = hololive_card.attr("href") else {
            eprintln!("Card url not found for {card_number:?}");
            return;
        };

        let card_url = Url::parse(url).unwrap().join(card_url).unwrap();

        let Some((_, id)) = card_url.query_pairs().into_owned().find(|(k, _)| k == "id") else {
            eprintln!("Card id not found for {card_number:?}");
            return;
        };
        if let Some(illust) = card
            .illustrations
            .iter_mut()
            .find(|i| i.manage_id == id.parse().ok() && i.illustrator.is_none())
        {
            let resp = http_client().get(card_url).send().unwrap();
            let content = resp.text().unwrap();

            // parse the content and update cards
            let document = Html::parse_document(&content);

            // get the illustrator from the card
            // save empty string to indicate that we know there is no illustrator. no need for request
            illust.illustrator = Some(
                document
                    .select(illustrator)
                    .next()
                    .map(|c| c.text().collect::<String>())
                    .unwrap_or_default(),
            );
        }
    }

    // Retrieve the following fields from Hololive official site:
    // - Card number
    // - Card name "JP"
    // - Type (e.g. Buzzホロメン)
    // - Tags "JP"
    // - Color
    // - LIFE/HP
    // - Bloom level
    // - Baton pass
    // - Text "JP"
    pub fn retrieve_card_info_from_hololive(all_cards: &mut CardsDatabase) {
        println!("Retrieve all cards info from Hololive official site");

        let updated_count = Arc::new(Mutex::new(0));
        let all_cards = Arc::new(RwLock::new(all_cards));

        // Process pages in parallel
        let _ = (1..)
            .par_bridge()
            .map({
                let updated_count = updated_count.clone();
                let all_cards = all_cards.clone();
                move |page| {
                    let url = "https://hololive-official-cardgame.com/cardlist/cardsearch_ex";
                    let resp = http_client()
                        .get(url)
                        .query(&[("view", "text"), ("page", page.to_string().as_str())])
                        .send()
                        .unwrap();

                    let content = resp.text().unwrap();

                    // no more card in this page
                    if content.contains(
                        "<title>hololive OFFICIAL CARD GAME｜ホロライブプロダクション</title>",
                    ) {
                        return None;
                    }

                    // parse the content and update cards
                    let document = Html::parse_document(&content);
                    static CARDS: OnceLock<Selector> = OnceLock::new();
                    let cards = CARDS.get_or_init(|| Selector::parse("li a").unwrap());
                    static CARD_NUMBER: OnceLock<Selector> = OnceLock::new();
                    let card_number =
                        CARD_NUMBER.get_or_init(|| Selector::parse(".number").unwrap());

                    let mut page_updated_count = 0;

                    for hololive_card in document.select(cards) {
                        let Some(card_number) = hololive_card
                            .select(card_number)
                            .next()
                            .map(|c| c.text().collect::<String>())
                        else {
                            println!("Card number not found");
                            continue;
                        };

                        let mut all_cards = all_cards.write();
                        let Some(card) = all_cards.get_mut(&card_number) else {
                            println!("Card {card_number:?} not found");
                            continue;
                        };

                        update_card_info(card, &hololive_card);
                        update_card_oshi_skills(card, &hololive_card);
                        update_card_keywords(card, &hololive_card);
                        update_card_arts(card, &hololive_card);
                        update_card_illustrations(card, &hololive_card, url);

                        page_updated_count += 1;
                    }

                    // Increment the total updated count
                    *updated_count.lock() += page_updated_count;

                    println!("Page {page} done: updated {page_updated_count} cards");

                    Some(())
                }
            })
            .while_some()
            .max(); // Need this to drive the iterator

        println!("Updated {} cards", *updated_count.lock());
    }
}
