use hocg_fan_sim_assets_model::{Art, Card, Extra, Keyword, Language, OshiSkill, Tag};

pub mod decklog {

    use std::{fmt::Display, str::FromStr, sync::Arc};

    use hocg_fan_sim_assets_model::{
        BloomLevel, CardIllustration, CardType, CardsDatabase, Language, Localized, SupportType,
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
                s if s.contains("推し") || s.contains("oshi") => CardType::OshiHoloMember,
                s if s.contains("ホロメン") || s.contains("holomem") => CardType::HoloMember,
                s if s.contains("スタッフ") || s.contains("staff") => {
                    CardType::Support(SupportType::Staff)
                }
                s if s.contains("アイテム") || s.contains("item") => {
                    CardType::Support(SupportType::Item)
                }
                s if s.contains("イベント") || s.contains("event") => {
                    CardType::Support(SupportType::Event)
                }
                s if s.contains("ツール") || s.contains("tool") => {
                    CardType::Support(SupportType::Tool)
                }
                s if s.contains("マスコット") || s.contains("mascot") => {
                    CardType::Support(SupportType::Mascot)
                }
                s if s.contains("ファン") || s.contains("fan") => {
                    CardType::Support(SupportType::Fan)
                }
                "エール" | "cheer" => CardType::Cheer,
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
        language: Language,
    ) -> Vec<(String, usize)> {
        if number_filter.is_none() && expansion.is_none() {
            println!("Retrieve ALL cards info from Deck Log for language: {language:?}");
        } else {
            println!(
                "Retrieve cards info from Deck Log with filters - language: {:?}, number: {}, expension: {}",
                language,
                number_filter.as_deref().unwrap_or("all"),
                expansion.as_deref().unwrap_or("all")
            );
        }

        let filtered_cards = Arc::new(Mutex::new(Vec::new()));
        let all_cards = Arc::new(RwLock::new(all_cards));
        let error_count = Arc::new(Mutex::new(0u8));

        let _ = ["N", "OSHI", "YELL"]
            .into_par_iter()
            .flat_map({
                let filtered_cards = filtered_cards.clone();
                let all_cards = all_cards.clone();
                let error_count = error_count.clone();
                move |deck_type| {
                    (1..)
                        .par_bridge()
                        .map({
                            let filtered_cards = filtered_cards.clone();
                            let all_cards = all_cards.clone();
                            let error_count = error_count.clone();
                            move |page| {
                                if *error_count.lock() >= 3 {
                                    return None;
                                }

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

                                let (url, referrer) = match language {
                                    Language::Japanese => (
                                        "https://decklog.bushiroad.com/system/app/api/search/9",
                                        "https://decklog.bushiroad.com/",
                                    ),
                                    Language::English => (
                                        "https://decklog-en.bushiroad.com/system/app/api/search/8",
                                        "https://decklog-en.bushiroad.com/",
                                    ),
                                };

                                let resp = http_client()
                                    .post(url)
                                    .header(REFERER, referrer)
                                    .json(&req)
                                    .send()
                                    .unwrap();

                                let content = resp.text().unwrap();
                                // println!("{content}");
                                let cards = serde_json::from_str(&content);
                                let Ok(cards): Result<Vec<DeckLogCard>, _> = cards else {
                                    eprintln!("didn't like response: {content}");
                                    let mut error_count = error_count.lock();
                                    *error_count += 1;
                                    if *error_count >= 3 {
                                        panic!("Too many errors, stopping.");
                                    }
                                    return Some(());
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

                                    // if the card number is empty, skip it
                                    if dl_card.card_number.is_empty()
                                        || dl_card.card_number == "null"
                                    {
                                        continue;
                                    }

                                    // fix Kazama Iroha oshi card number (deck log has a bug)
                                    if dl_card.manage_id == Some(532) {
                                        dl_card.card_number = "hSD06-001".into();
                                    }

                                    // remove the old manage_id if it exists
                                    all_cards
                                        .write()
                                        .values_mut()
                                        .flat_map(|cs| cs.illustrations.iter_mut())
                                        .filter(|c| {
                                            c.card_number != dl_card.card_number
                                                && c.manage_id
                                                    .value(language)
                                                    .iter()
                                                    .flatten()
                                                    .any(|m| Some(m) == dl_card.manage_id.as_ref())
                                        })
                                        .for_each(|c| {
                                            if let Some(manage_id) = c.manage_id.value_mut(language)
                                                && let Some(id) = dl_card.manage_id
                                            {
                                                manage_id.retain(|&m| m != id);
                                            }
                                            // remove empty manage_id
                                            c.manage_id
                                                .value_mut(language)
                                                .take_if(|v| v.is_empty());
                                        });

                                    // add the card the list
                                    let mut all_cards = all_cards.write();
                                    let card: &mut hocg_fan_sim_assets_model::Card =
                                        all_cards.entry(dl_card.card_number.clone()).or_default();
                                    card.card_number = dl_card.card_number.clone();
                                    *card.name.value_mut(language) = Some(dl_card.name.clone());
                                    card.card_type = dl_card.card_type();
                                    if let CardType::OshiHoloMember = card.card_type {
                                        *card.max_amount.value_mut(language) =
                                            Some(dl_card.max.min(1));
                                    } else {
                                        *card.max_amount.value_mut(language) = Some(dl_card.max);
                                    }
                                    card.bloom_level = dl_card.bloom_level();
                                    card.buzz = dl_card.buzz();
                                    card.limited = dl_card.limited();

                                    let illustrations = &mut card.illustrations;
                                    // find the card, first by manage_id, then by image, then overwrite delta, otherwise just add
                                    if let Some(illust) = {
                                        if let Some(i) = illustrations.iter_mut().find(|i| {
                                            i.manage_id
                                                .value(language)
                                                .iter()
                                                .flatten()
                                                .any(|m| Some(m) == dl_card.manage_id.as_ref())
                                        }) {
                                            Some(i)
                                        } else {
                                            illustrations
                                                .iter_mut()
                                                .find(|c| !c.manage_id.has_value())
                                        }
                                    } {
                                        // only these fields are retrieved
                                        illust.card_number = dl_card.card_number;
                                        if let Some(manage_id) = dl_card.manage_id {
                                            illust
                                                .manage_id
                                                .value_mut(language)
                                                .get_or_insert_default()
                                                .push(manage_id);
                                            if let Some(ids) =
                                                illust.manage_id.value_mut(language).as_mut()
                                            {
                                                ids.sort();
                                                ids.dedup();
                                            }
                                        }
                                        illust.rarity = dl_card.rare;
                                        *illust.img_path.value_mut(language) = Some(dl_card.img);
                                    } else {
                                        let illust = CardIllustration {
                                            card_number: dl_card.card_number.clone(),
                                            manage_id: match language {
                                                Language::Japanese => Localized {
                                                    japanese: dl_card.manage_id.map(|id| vec![id]),
                                                    ..Default::default()
                                                },
                                                Language::English => Localized {
                                                    english: dl_card.manage_id.map(|id| vec![id]),
                                                    ..Default::default()
                                                },
                                            },
                                            rarity: dl_card.rare,
                                            img_path: Localized::new(language, dl_card.img),
                                            ..Default::default()
                                        };
                                        // add the card to the list
                                        illustrations.push(illust);
                                    }

                                    // sort the list, by oldest to latest
                                    illustrations.sort_by_cached_key(|c| c.manage_id.clone());

                                    // add to filtered cards
                                    if let Some(manage_id) = dl_card.manage_id {
                                        filtered_cards.lock().push(manage_id);
                                    }
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
            .filter(|c| {
                filtered_cards.iter().any(|f| {
                    c.1.manage_id
                        .value(language)
                        .iter()
                        .flatten()
                        .any(|m| m == f)
                })
            })
            .map(|c| (c.1.card_number.clone(), c.0))
            .collect()
    }
}

pub mod hololive_official {
    use std::sync::Arc;
    use std::{ops::Deref, sync::OnceLock};

    use hocg_fan_sim_assets_model::{
        Art, ArtPower, BloomLevel, Card, CardType, CardsDatabase, Color, Extra, Keyword,
        KeywordEffect, Language, Localized, OshiSkill,
    };
    use itertools::Itertools;
    use parking_lot::{Mutex, RwLock};
    use rayon::iter::{ParallelBridge, ParallelIterator};
    use reqwest::Url;
    use reqwest::header::REFERER;
    use scraper::{Html, Node, Selector};

    use crate::data::{
        update_arts, update_extra, update_keywords, update_oshi_skills, update_tags,
    };
    use crate::http_client;
    use crate::utils::TrimOnce;

    fn card_type_from_str(card_type: &str) -> CardType {
        match card_type.trim().to_lowercase().as_str() {
            s if s.contains("推し") || s.contains("oshi") => CardType::OshiHoloMember,
            s if s.contains("ホロメン") || s.contains("holomem") => CardType::HoloMember,
            s if s.contains("スタッフ") || s.contains("staff") => {
                CardType::Support(hocg_fan_sim_assets_model::SupportType::Staff)
            }
            s if s.contains("アイテム") || s.contains("item") => {
                CardType::Support(hocg_fan_sim_assets_model::SupportType::Item)
            }
            s if s.contains("イベント") || s.contains("event") => {
                CardType::Support(hocg_fan_sim_assets_model::SupportType::Event)
            }
            s if s.contains("ツール") || s.contains("tool") => {
                CardType::Support(hocg_fan_sim_assets_model::SupportType::Tool)
            }
            s if s.contains("マスコット") || s.contains("mascot") => {
                CardType::Support(hocg_fan_sim_assets_model::SupportType::Mascot)
            }
            s if s.contains("ファン") || s.contains("fan") => {
                CardType::Support(hocg_fan_sim_assets_model::SupportType::Fan)
            }
            "エール" | "cheer" => CardType::Cheer,
            _ => CardType::Other,
        }
    }

    fn buzz(card_type: &str) -> bool {
        card_type.trim().to_lowercase().contains("buzz")
    }

    fn limited(card_type: &str) -> bool {
        card_type.trim().to_lowercase().contains("limited")
    }

    fn tags_from_str(tags: &str, language: Language) -> Vec<Localized<String>> {
        tags.split('#')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| Localized::new(language, format!("#{s}")))
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

    fn update_card_info(card: &mut Card, hololive_card: &scraper::ElementRef, language: Language) {
        static CARD_NAME: OnceLock<Selector> = OnceLock::new();
        let card_name = CARD_NAME.get_or_init(|| Selector::parse(".name").unwrap());

        // Card name
        let card_name = hololive_card
            .select(card_name)
            .next()
            .map(|c| c.text().collect::<String>());
        // from Deck Log
        if card.name.value(language).is_none() {
            *card.name.value_mut(language) = card_name;
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
                "カードタイプ" | "card type" => {
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
                "タグ" | "tag" => {
                    update_tags(card, tags_from_str(value, language), language, false);
                }
                "色" | "color" => {
                    let mut colors = colors_from_str(value);
                    fix_colors(card, &mut colors);
                    card.colors = colors;
                }
                "life" => card.life = value.parse().unwrap_or_default(),
                "hp" => card.hp = value.parse().unwrap_or_default(),
                "bloomレベル" | "bloom level" => {
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
                "バトンタッチ" | "baton pass" => card.baton_pass = baton_pass_from_str(value),
                "能力テキスト" | "ability text" => {
                    *card.ability_text.value_mut(language) = Some(
                        value
                            .lines()
                            .map(|l| l.trim())
                            // should remove the "LIMITED：ターンに１枚しか使えない。" line
                            .filter(|l| !l.to_lowercase().starts_with("limited"))
                            .collect_vec()
                            .join("\n")
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
        let mut extra = hololive_card
            .select(extra)
            .next()
            .map(|c| Localized::new(language, c.text().collect::<String>()));
        fix_extra(card, &mut extra, language);
        update_extra(card, extra, language, false);
    }

    fn update_card_oshi_skills(
        card: &mut Card,
        hololive_card: &scraper::ElementRef,
        language: Language,
    ) {
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
                    .trim_start_once("[ホロパワー：")
                    .trim_start_once("[holo Power：")
                    .trim_start_once("-")
                    .trim_end_once("]")
                    .trim_end_once("消費")
                    .to_string()
                    .try_into()
                    .unwrap_or_default(),
                name: Localized::new(language, name.trim().to_string()),
                ability_text: Localized::new(language, text.trim().to_string()),
            };
            oshi_skills.push(oshi_skill);
        }
        // replace existing skills. will need to import english skills later
        update_oshi_skills(card, oshi_skills, language, false);
    }

    fn update_card_keywords(
        card: &mut Card,
        hololive_card: &scraper::ElementRef,
        language: Language,
    ) {
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
                } else if let Node::Element(el) = node.value()
                    && let Some(alt) = el.attr("alt")
                {
                    keyword_text.push_str(alt.to_string().as_str());
                    keyword_text.push('\n');
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
                effect: match effect.to_lowercase().trim() {
                    "コラボエフェクト" | "collab effect" => KeywordEffect::Collab,
                    "ブルームエフェクト" | "bloom effect" => KeywordEffect::Bloom,
                    "ギフト" | "gift" => KeywordEffect::Gift,
                    _ => KeywordEffect::Other,
                },
                name: Localized::new(language, name.trim().to_string()),
                ability_text: Localized::new(language, text.trim().to_string()),
            };
            keywords.push(keyword);
        }
        // replace existing keywords. will need to import english keywords later
        update_keywords(card, keywords, language, false);
    }

    fn update_card_arts(card: &mut Card, hololive_card: &scraper::ElementRef, language: Language) {
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
                } else if let Node::Element(el) = node.value()
                    && let Some(alt) = el.attr("alt")
                {
                    art_text.push_str(alt.to_string().as_str());
                    art_text.push('\n');
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
                name: Localized::new(language, name.trim().to_string()),
                power: power.to_string().try_into().unwrap_or_default(),
                advantage,
                ability_text: text.map(|text| Localized::new(language, text.trim().to_string())),
            };
            arts.push(art);
        }
        // replace existing arts. will need to import english arts later
        fix_arts(card, &mut arts);
        update_arts(card, arts, language, false);
    }

    fn update_card_illustrations(
        card: &mut Card,
        card_url: Url,
        card_id: &str,
        language: Language,
    ) {
        static ILLUSTRATOR: OnceLock<Selector> = OnceLock::new();
        let illustrator = ILLUSTRATOR.get_or_init(|| Selector::parse(".ill-name span").unwrap());

        if let Some(illust) = card.illustrations.iter_mut().find(|i| {
            i.illustrator.is_none()
                && i.manage_id
                    .value(language)
                    .iter()
                    .flatten()
                    .any(|m| Some(*m) == card_id.parse().ok())
        }) {
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

    // --- Fixes for known issues in the official database ---
    fn fix_card_number(card_id: &str, card_number: &mut String) {
        // hSD06-001 Kazama Iroha oshi card number (the official db has a bug, it's the second time it happened)
        if card_id == "532" {
            *card_number = "hSD06-001".into();
        }
    }

    fn fix_colors(card: &mut Card, colors: &mut Vec<Color>) {
        // hSD01-002 AZKi oshi card color (the official EN db has a bug)
        if card.card_number == "hSD01-002" {
            *colors = vec![Color::Green];
        }
    }

    fn fix_arts(card: &mut Card, arts: &mut [Art]) {
        // hBP05-061 Nerissa Ravencroft's art Unleashed Charm should be 120+
        if card.card_number == "hBP05-061"
            && let Some(art) = arts
                .iter_mut()
                .find(|a| a.name.value(Language::Japanese).as_deref() == Some("Unleashed Charm"))
        {
            art.power = ArtPower::Plus(120);
        }
    }

    fn fix_extra(card: &Card, extra: &mut Option<Extra>, language: Language) {
        // hSD08-002 Amane Kanata does not have extra text
        if card.card_number == "hSD08-002" {
            *extra = None;
        }

        // hBP05-082 Aki Rosenthal's Axe does have extra text
        if card.card_number == "hBP05-082" && language == Language::Japanese {
            *extra = Some(Localized::jp("このツールは〈石の斧〉としても扱う".into()));
        }
    }
    // --- End of fixes ---

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
    pub fn retrieve_card_info_from_hololive(all_cards: &mut CardsDatabase, language: Language) {
        println!("Retrieve all cards info from Hololive official site for language: {language:?}");

        let updated_count = Arc::new(Mutex::new(0));
        let all_cards = Arc::new(RwLock::new(all_cards));

        // Process pages in parallel
        let _ = (1..)
            .par_bridge()
            .map({
                let updated_count = updated_count.clone();
                let all_cards = all_cards.clone();
                move |page| {
                    let (url, referrer) = match language {
                        Language::Japanese => (
                            "https://hololive-official-cardgame.com/cardlist/cardsearch_ex",
                            "https://hololive-official-cardgame.com/",
                        ),
                        Language::English => (
                            "https://en.hololive-official-cardgame.com/cardlist/cardsearch_ex",
                            "https://en.hololive-official-cardgame.com/",
                        ),
                    };

                    let resp = http_client()
                        .get(url)
                        .query(&[("view", "text"), ("page", page.to_string().as_str())])
                        .header(REFERER, referrer)
                        .send()
                        .unwrap();

                    let content = resp.text().unwrap();

                    // no more card in this page
                    if content.contains(
                        "<title>hololive OFFICIAL CARD GAME｜ホロライブプロダクション</title>",
                    ) || content
                        .contains("<title>hololive OFFICIAL CARD GAME｜hololive production</title>")
                        || content.contains("Page not found")
                    {
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
                        let Some(mut card_number) = hololive_card
                            .select(card_number)
                            .next()
                            .map(|c| c.text().collect::<String>())
                        else {
                            println!("Card number not found");
                            return None;
                        };

                        // need to go to the detailed page
                        let Some(card_url) = hololive_card.attr("href") else {
                            eprintln!("Card url not found for {card_number:?}");
                            continue;
                        };
                        let card_url = Url::parse(url).unwrap().join(card_url).unwrap();
                        let Some((_, card_id)) =
                            card_url.query_pairs().into_owned().find(|(k, _)| k == "id")
                        else {
                            eprintln!("Card id not found for {card_number:?}");
                            continue;
                        };

                        // fix card number if needed
                        fix_card_number(&card_id, &mut card_number);

                        let _all_cards = all_cards.read();
                        let Some(card) = _all_cards.get(&card_number) else {
                            println!("Card {card_number:?} not found");
                            continue;
                        };
                        let mut card = card.clone();
                        drop(_all_cards);

                        update_card_info(&mut card, &hololive_card, language);
                        update_card_oshi_skills(&mut card, &hololive_card, language);
                        update_card_keywords(&mut card, &hololive_card, language);
                        update_card_arts(&mut card, &hololive_card, language);
                        update_card_illustrations(&mut card, card_url, &card_id, language);

                        let mut _all_cards = all_cards.write();
                        _all_cards.insert(card_number, card);

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

pub fn update_oshi_skills(
    card: &mut Card,
    mut oshi_skills: Vec<OshiSkill>,
    language: Language,
    only_text: bool,
) {
    let orig_oshi_skills = &mut card.oshi_skills;
    let new_oshi_skills = &mut oshi_skills;
    let mut copy_language = language;

    if only_text {
        // If the number of sheet oshi skills is different from the original, warn
        if new_oshi_skills.len() != orig_oshi_skills.len() {
            eprintln!(
                "Warning: {} has a different number of oshi skills than in the original card: {} should be {}",
                card.card_number,
                new_oshi_skills.len(),
                orig_oshi_skills.len()
            );
        }
    } else {
        // Use the new oshi skills as base and copy the other language from the original
        copy_language = match language {
            Language::Japanese => Language::English,
            Language::English => Language::Japanese,
        };
        // Fully replace the original oshi skills with the new ones
        std::mem::swap(orig_oshi_skills, new_oshi_skills);
    }

    for (orig_skill, new_skill) in orig_oshi_skills.iter_mut().zip(new_oshi_skills.iter_mut()) {
        if orig_skill.special != new_skill.special {
            eprintln!(
                "Warning: {} oshi skill special mismatch: {} should be {}",
                card.card_number, new_skill.special, orig_skill.special
            );
        }

        if orig_skill.holo_power != new_skill.holo_power {
            eprintln!(
                "Warning: {} oshi skill holo power mismatch: {} should be {}",
                card.card_number,
                String::from(new_skill.holo_power),
                String::from(orig_skill.holo_power)
            );
        }

        // Update the text in the specified language
        *orig_skill.name.value_mut(copy_language) = new_skill.name.value_mut(copy_language).take();
        *orig_skill.ability_text.value_mut(copy_language) =
            new_skill.ability_text.value_mut(copy_language).take();
    }
}

pub fn update_keywords(
    card: &mut Card,
    mut keywords: Vec<Keyword>,
    language: Language,
    only_text: bool,
) {
    let orig_keywords = &mut card.keywords;
    let new_keywords = &mut keywords;
    let mut copy_language = language;

    if only_text {
        // If the number of sheet keywords is different from the original, warn
        if new_keywords.len() != orig_keywords.len() {
            eprintln!(
                "Warning: {} has a different number of keywords than in the original card: {} should be {}",
                card.card_number,
                new_keywords.len(),
                orig_keywords.len()
            );
        }
    } else {
        // Use the new keywords as base and copy the other language from the original
        copy_language = match language {
            Language::Japanese => Language::English,
            Language::English => Language::Japanese,
        };
        // Fully replace the original keywords with the new ones
        std::mem::swap(orig_keywords, new_keywords);
    }

    for (orig_keyword, new_keyword) in orig_keywords.iter_mut().zip(new_keywords.iter_mut()) {
        if orig_keyword.effect != new_keyword.effect {
            eprintln!(
                "Warning: {} keyword effect mismatch: {:?} should be {:?}",
                card.card_number, new_keyword.effect, orig_keyword.effect
            );
        }

        // Update the text in the specified language
        *orig_keyword.name.value_mut(copy_language) =
            new_keyword.name.value_mut(copy_language).take();
        *orig_keyword.ability_text.value_mut(copy_language) =
            new_keyword.ability_text.value_mut(copy_language).take();
    }
}

pub fn update_arts(card: &mut Card, mut arts: Vec<Art>, language: Language, only_text: bool) {
    let orig_arts = &mut card.arts;
    let new_arts = &mut arts;
    let mut copy_language = language;

    if only_text {
        // If the number of sheet arts is different from the original, warn
        if new_arts.len() != orig_arts.len() {
            eprintln!(
                "Warning: {} has a different number of arts than in the original card: {} should be {}",
                card.card_number,
                new_arts.len(),
                orig_arts.len()
            );
        }
    } else {
        // Use the new arts as base and copy the other language from the original
        copy_language = match language {
            Language::Japanese => Language::English,
            Language::English => Language::Japanese,
        };
        // Fully replace the original arts with the new ones
        std::mem::swap(orig_arts, new_arts);
    }

    for (orig_art, new_art) in orig_arts.iter_mut().zip(new_arts.iter_mut()) {
        if orig_art.cheers != new_art.cheers {
            eprintln!(
                "Warning: {} art cheers mismatch: {:?} should be {:?}",
                card.card_number, new_art.cheers, orig_art.cheers
            );
        }

        if orig_art.power != new_art.power {
            eprintln!(
                "Warning: {} art power mismatch: {:?} should be {:?}",
                card.card_number, new_art.power, orig_art.power
            );
        }

        if orig_art.advantage != new_art.advantage {
            eprintln!(
                "Warning: {} art advantage mismatch: {:?} should be {:?}",
                card.card_number, new_art.advantage, orig_art.advantage
            );
        }

        // Update the text in the specified language
        *orig_art.name.value_mut(copy_language) = new_art.name.value_mut(copy_language).take();
        if let Some(orig_ability_text) = &mut orig_art.ability_text {
            if let Some(new_ability_text) = &mut new_art.ability_text {
                *orig_ability_text.value_mut(copy_language) =
                    new_ability_text.value_mut(copy_language).take();
            } else {
                eprintln!(
                    "Warning: {} art ability text mismatch: {:?} should be {:?}",
                    card.card_number, new_art.ability_text, orig_ability_text
                );
            }
        } else if new_art.ability_text.is_some() {
            eprintln!(
                "Warning: {} art ability text mismatch: {:?} should be {:?}",
                card.card_number, new_art.ability_text, orig_art.ability_text
            );
        }
    }
}

pub fn update_tags(card: &mut Card, mut tags: Vec<Tag>, language: Language, only_text: bool) {
    let orig_tags = &mut card.tags;
    let new_tags = &mut tags;
    let mut copy_language = language;

    if only_text {
        // If the number of sheet tags is different from the original, warn
        if new_tags.len() != orig_tags.len() {
            eprintln!(
                "Warning: {} has a different number of tags than in the original card: {} should be {}",
                card.card_number,
                new_tags.len(),
                orig_tags.len()
            );
        }
    } else {
        // Use the new tags as base and copy the other language from the original
        copy_language = match language {
            Language::Japanese => Language::English,
            Language::English => Language::Japanese,
        };
        // Fully replace the original tags with the new ones
        std::mem::swap(orig_tags, new_tags);
    }

    for (orig_tag, new_tag) in orig_tags.iter_mut().zip(new_tags.iter_mut()) {
        // Update the text in the specified language
        *orig_tag.value_mut(copy_language) = new_tag.value_mut(copy_language).take();
    }
}

pub fn update_extra(
    card: &mut Card,
    mut extra: Option<Extra>,
    language: Language,
    only_text: bool,
) {
    let orig_extra = &mut card.extra;
    let new_extra = &mut extra;
    let mut copy_language = language;

    if only_text {
        // If the number of sheet extra is different from the original, warn
        if new_extra.is_some() != orig_extra.is_some() {
            eprintln!(
                "Warning: {} has a different extra than in the original card: {} should be {}",
                card.card_number,
                new_extra.is_some(),
                orig_extra.is_some()
            );
        }
    } else {
        // Use the new extra as base and copy the other language from the original
        copy_language = match language {
            Language::Japanese => Language::English,
            Language::English => Language::Japanese,
        };
        // Fully replace the original extra with the new one
        std::mem::swap(orig_extra, new_extra);
    }

    for (orig_extra, new_extra) in orig_extra.iter_mut().zip(new_extra.iter_mut()) {
        *orig_extra.value_mut(copy_language) = new_extra.value_mut(copy_language).take();
    }
}
