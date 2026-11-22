use hocg_fan_sim_assets_model::{Art, Card, Extra, Keyword, Language, OshiSkill, Tag};

pub mod deck_log {

    use std::{
        collections::HashSet, fmt::Display, fs, path::Path, str::FromStr, sync::Arc, time::Duration,
    };

    use hocg_fan_sim_assets_model::{
        BloomLevel, CardIllustration, CardType, CardsDatabase, Language, Localized, SupportType,
    };
    use parking_lot::{Mutex, RwLock};
    use rayon::iter::{IntoParallelIterator, ParallelBridge, ParallelIterator};
    use reqwest::header::REFERER;
    use serde::{Deserialize, Deserializer, Serialize};

    use crate::{body_excerpt, deck_log_en_http_client, deck_log_http_client, warn_skipped};

    /// After that many Deck Log pages that can't be read, Deck Log is considered down for the
    /// language (all deck types).
    const MAX_DECK_LOG_ERRORS: u8 = 3;
    /// How long to wait before trying a page again when its response isn't the cards (an empty
    /// body or an HTML page). The http client only retries server errors and network errors.
    const DECK_LOG_RETRY_DELAYS: [Duration; 2] = [Duration::from_secs(5), Duration::from_secs(10)];

    /// Retrieves a page of Deck Log cards with `send`, trying again after each of `retry_delays`
    /// when the response can't be read. The error says the status and the start of the body.
    fn fetch_deck_log_page(
        send: impl Fn() -> reqwest::Result<reqwest::blocking::Response>,
        retry_delays: &[Duration],
    ) -> Result<Vec<DeckLogCard>, String> {
        let mut delays = retry_delays.iter();
        loop {
            // network errors are already retried by the client
            let resp = send().map_err(|error| error.to_string())?;
            let status = resp.status();
            let error = match resp.text() {
                Ok(content) => match serde_json::from_str(&content) {
                    Ok(cards) => return Ok(cards),
                    Err(_) => format!(
                        "HTTP {status}, didn't like response: {}",
                        body_excerpt(&content)
                    ),
                },
                Err(error) => format!("HTTP {status}, {error}"),
            };
            let Some(delay) = delays.next() else {
                return Err(error);
            };
            eprintln!("Deck Log page not read ({error}), trying again in {delay:?}");
            std::thread::sleep(*delay);
        }
    }

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

        fn fix_card(&mut self, language: Language) {
            // fix issues with Deck Log Japanese site
            if language == Language::Japanese {
                // fix Kazama Iroha oshi card number
                if self.manage_id == Some(532) {
                    self.card_number = "hSD06-001".into();
                }

                // fix Nerissa Ravencroft oshi card HR image path
                if self.manage_id == Some(1373) {
                    self.img = "hBP05/hBP02-065_HR.png".into();
                }

                // fix Isaki Riona bloom level
                if self.card_number == "hSD10-010" {
                    self.bloom_level = "Spot".into();
                }

                // fix Vestia Zeta buzz
                if self.card_number == "hBP07-019" {
                    self.card_kind = "Buzzホロメン".into();
                }

                // fix Elizabeth Rose Bloodflame buzz
                if self.card_number == "hBP07-048" {
                    self.card_kind = "Buzzホロメン".into();
                }

                // fix Nerissa Ravencroft buzz
                if self.card_number == "hBP07-076" {
                    self.card_kind = "Buzzホロメン".into();
                }
            }

            // fix issues with Deck Log English site
            if language == Language::English {
                // fix Mikkorone 24 support SEC image path
                if self.manage_id == Some(419) {
                    self.img = "hBP02/EN_hBP02-084_SEC.png".into();
                }
            }
        }
    }

    // Retrieve the following fields from Deck Log:
    // - Card number
    // - Manage ID (unique id)
    // - Rarity
    // - Image URL
    // - Max amount
    // - Deck type (oshi, cheer, etc)
    pub fn retrieve_card_info_from_deck_log(
        all_cards: &mut CardsDatabase,
        number_filter: &Option<String>,
        expansion: &Option<String>,
        optimized_original_images: bool,
        images_jp_path: &Path,
        images_en_path: &Path,
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

        let images_path = match language {
            Language::Japanese => images_jp_path,
            Language::English => images_en_path,
        };

        let filtered_cards = Arc::new(Mutex::new(HashSet::new()));
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
                                if *error_count.lock() >= MAX_DECK_LOG_ERRORS {
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

                                let (client, url, referrer) = match language {
                                    Language::Japanese => (
                                        deck_log_http_client(),
                                        "https://decklog.bushiroad.com/system/app/api/search/9",
                                        "https://decklog.bushiroad.com/",
                                    ),
                                    Language::English => (
                                        deck_log_en_http_client(),
                                        "https://decklog-en.bushiroad.com/system/app/api/search/8",
                                        "https://decklog-en.bushiroad.com/",
                                    ),
                                };

                                // a page that can't be read is skipped (its cards keep their
                                // data); after too many, Deck Log is considered down for the
                                // language: the deck types share the count
                                let cards = match fetch_deck_log_page(
                                    || {
                                        client
                                            .post(url)
                                            .header(REFERER, referrer)
                                            .json(&req)
                                            .send()
                                    },
                                    &DECK_LOG_RETRY_DELAYS,
                                ) {
                                    Ok(cards) => cards,
                                    Err(error) => {
                                        eprintln!(
                                            "Warning: Deck Log {deck_type} page {page} skipped: {error}"
                                        );
                                        let mut error_count = error_count.lock();
                                        *error_count += 1;
                                        if *error_count == MAX_DECK_LOG_ERRORS {
                                            warn_skipped(&format!(
                                                "Deck Log unavailable for {language:?} ({error}), skipped the remaining pages; their cards keep their data"
                                            ));
                                        }
                                        return if *error_count >= MAX_DECK_LOG_ERRORS {
                                            None
                                        } else {
                                            Some(())
                                        };
                                    }
                                };

                                // no more card in this page
                                if cards.is_empty() {
                                    return None;
                                }

                                // update records with deck type and webp images
                                for mut dl_card in cards {
                                    // fix the card (deck log has bugs)
                                    dl_card.fix_card(language);

                                    if !optimized_original_images {
                                        dl_card.img = dl_card.img.replace(".png", ".webp");
                                    }

                                    // if the card number is empty, skip it
                                    if dl_card.card_number.is_empty()
                                        || dl_card.card_number == "null"
                                    {
                                        continue;
                                    }

                                    // remove the old manage_id, if it exists
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

                                    // add the card to the list
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
                                    // find the card, first by manage_id, then by image, otherwise just add
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
                                            illustrations.iter_mut().find(|i| {
                                                i.img_path.value(language).as_ref()
                                                    == Some(&dl_card.img)
                                            })
                                        }
                                    } {
                                        // only these fields are retrieved
                                        illust.card_number = dl_card.card_number;
                                        if let Some(manage_id) = dl_card.manage_id {
                                            illust
                                                .manage_id
                                                .value_mut(language)
                                                .get_or_insert_default()
                                                .insert(manage_id);
                                        }
                                        illust.rarity = dl_card.rare.into();
                                        // rename the old file
                                        if let Some(img_path) =
                                            illust.img_path.value_mut(language).as_mut()
                                            && dl_card.img != *img_path
                                        {
                                            let old_path = images_path.join(&img_path);
                                            *img_path = dl_card.img;
                                            let new_path = images_path.join(&img_path);
                                            if fs::exists(&old_path).unwrap_or_default() {
                                                fs::create_dir_all(new_path.parent().unwrap())
                                                    .unwrap();
                                                fs::copy(old_path, new_path).unwrap();
                                            }
                                            *illust.img_last_modified.value_mut(language) = None;
                                        } else {
                                            *illust.img_path.value_mut(language) =
                                                Some(dl_card.img);
                                        }
                                    } else {
                                        let illust = CardIllustration {
                                            card_number: dl_card.card_number.clone(),
                                            manage_id: match language {
                                                Language::Japanese => Localized {
                                                    japanese: dl_card
                                                        .manage_id
                                                        .map(|id| [id].into()),
                                                    ..Default::default()
                                                },
                                                Language::English => Localized {
                                                    english: dl_card
                                                        .manage_id
                                                        .map(|id| [id].into()),
                                                    ..Default::default()
                                                },
                                            },
                                            rarity: dl_card.rare.into(),
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
                                        filtered_cards.lock().insert(manage_id);
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
                c.1.manage_id
                    .value(language)
                    .iter()
                    .flatten()
                    .any(|m| filtered_cards.contains(m))
            })
            .map(|c| (c.1.card_number.clone(), c.0))
            .collect()
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::test_server::{http, serve};

        fn fetch(url: &str, retry_delays: &[Duration]) -> Result<Vec<DeckLogCard>, String> {
            fetch_deck_log_page(|| crate::http_client().post(url).send(), retry_delays)
        }

        #[test]
        fn test_fetch_deck_log_page() {
            let cards = r#"[{"manage_id":"1","card_number":"hSD01-001","card_kind":"推しホロメン","name":"ときのそら","rare":"OSR","img":"hSD01/hSD01-001_OSR.png","bloom_level":"","max":"1"}]"#;
            let no_delay = [Duration::ZERO; 2];

            // empty body, then a maintenance page, then the cards
            let url = serve(vec![
                http("200 OK", ""),
                http(
                    "503 Service Unavailable",
                    "<title>メンテナンス中 [Maintenance] | Deck Log</title>",
                ),
                http("200 OK", cards),
            ]);
            let read = fetch(&url, &no_delay).unwrap();
            assert_eq!(read.len(), 1);
            assert_eq!(read[0].card_number, "hSD01-001");

            // no more cards
            let url = serve(vec![http("200 OK", "[]")]);
            assert!(fetch(&url, &no_delay).unwrap().is_empty());

            // still not the cards after the retries: the status and the start of the page
            let maintenance = format!(
                "<title>メンテナンス中 [Maintenance] | Deck Log</title><svg>{}</svg>",
                "<path/>".repeat(1000)
            );
            let url = serve(vec![http("200 OK", &maintenance); 3]);
            let error = fetch(&url, &no_delay).unwrap_err();
            assert!(
                error.starts_with("HTTP 200 OK, didn't like response: \"<title>メンテナンス中"),
                "{error}"
            );
            assert!(error.len() < 300, "{error}");

            // without retries
            let url = serve(vec![http("200 OK", ""), http("200 OK", cards)]);
            let error = fetch(&url, &[]).unwrap_err();
            assert_eq!(error, "HTTP 200 OK, didn't like response: empty body");

            // nothing listening
            let url = {
                let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                format!("http://{}/", listener.local_addr().unwrap())
            };
            assert!(fetch(&url, &no_delay).is_err());
        }
    }
}

pub mod hololive_official {
    use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
    use std::fs;
    use std::ops::Deref;
    use std::path::Path;
    use std::sync::LazyLock;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use hocg_fan_sim_assets_model::{
        Art, ArtPower, BloomLevel, Card, CardIllustration, CardType, CardsDatabase, Color, Extra,
        HoloPower, Keyword, KeywordEffect, Language, Localized, OshiSkill, OshiSkillKind,
        ProductsDatabase,
    };
    use itertools::Itertools;

    use rayon::iter::{ParallelBridge, ParallelIterator};
    use reqwest::Url;
    use reqwest::header::REFERER;
    use scraper::{Html, Node, Selector};

    use crate::data::{
        update_arts, update_extra, update_keywords, update_oshi_skills, update_tags,
    };
    use crate::products::{
        ListedProduct, ProductEntry, catalog_names, fix_product_urls, needs_details,
        update_catalog, update_printings, update_short_names,
    };
    use crate::utils::{TrimOnce, clean_text};
    use crate::{http_client, warn_skipped};

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
            .map(clean_text)
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
        static CARD_NAME: LazyLock<Selector> = LazyLock::new(|| Selector::parse(".name").unwrap());

        // Card name
        let card_name = hololive_card
            .select(&CARD_NAME)
            .next()
            .map(|c| c.text().collect::<String>())
            .map(|n| clean_text(&n));
        *card.name.value_mut(language) = card_name;

        static INFO: LazyLock<Selector> =
            LazyLock::new(|| Selector::parse(".info dl :is(dt, dd)").unwrap());

        // Card info
        let mut card_info: HashMap<String, String> = hololive_card
            .select(&INFO)
            .tuples()
            .map(|(key, value)| {
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
                (key.to_lowercase(), value.to_owned())
            })
            .collect();

        // add potential missing tag field (will pass through fixes in match)
        if !card_info.contains_key("タグ") && !card_info.contains_key("tag") {
            card_info.insert("tag".into(), "".into());
        }

        for (key, value) in card_info.iter() {
            match key.as_str() {
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
                        let mut buzz = buzz(value);
                        fix_buzz(card, &mut buzz);
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
                    let mut tags = tags_from_str(value, language);
                    fix_tags(card, &mut tags, language);
                    update_tags(card, tags, language, true, true);
                }
                "色" | "color" => {
                    let mut colors = colors_from_str(value);
                    fix_colors(card, &mut colors);
                    card.colors = colors;
                }
                "life" => card.life = value.parse().unwrap_or_default(),
                "hp" => card.hp = value.parse().unwrap_or_default(),
                "bloomレベル" | "bloom level" => {
                    let mut bloom_level = bloom_level_from_str(value);
                    fix_bloom_level(card, &mut bloom_level);

                    // from Deck Log
                    if card.bloom_level == Default::default() {
                        card.bloom_level = bloom_level;
                    } else {
                        // warn if the level is different
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
                    let text = value
                        .lines()
                        .map(|l| l.trim())
                        // should remove the "LIMITED：ターンに１枚しか使えない。" line
                        .filter(|l| !l.to_lowercase().starts_with("limited"))
                        .collect_vec()
                        .join("\n")
                        .trim()
                        .to_string();
                    *card.ability_text.value_mut(language) =
                        Some(fix_ability_text(&text, language));
                }
                _ => { /* nothing else */ }
            }
        }

        static EXTRA: LazyLock<Selector> =
            LazyLock::new(|| Selector::parse(".extra p:nth-child(2)").unwrap());

        // Extra
        let mut extra = hololive_card
            .select(&EXTRA)
            .next()
            .map(|c| c.text().collect::<String>())
            .map(|c| clean_text(&c))
            .map(|c| Localized::new(language, c));
        fix_extra(card, &mut extra, language);
        // Update extra only for non-support cards, it's not shown in the summary list
        if !matches!(card.card_type, CardType::Support(_)) {
            update_extra(card, extra, language, true, true);
        }

        fix_max_amount(card, language);
    }

    fn update_card_oshi_skills(
        card: &mut Card,
        hololive_card: &scraper::ElementRef,
        language: Language,
    ) {
        static SKILL: LazyLock<Selector> =
            LazyLock::new(|| Selector::parse(".skill p:nth-child(2)").unwrap());

        let card_number = &card.card_number;

        let mut oshi_skills = vec![];
        for oshi_skill in hololive_card.select(&SKILL) {
            let classes = oshi_skill
                .parent()
                .into_iter()
                .flat_map(|n| n.value().as_element().and_then(|el| el.attr("class")))
                .flat_map(str::split_whitespace)
                .collect_vec();

            let kind = if classes.contains(&"oshi") {
                OshiSkillKind::Normal
            } else if classes.contains(&"sp") {
                OshiSkillKind::Special
            } else if classes.contains(&"stage") {
                OshiSkillKind::Stage
            } else {
                eprintln!("Oshi skill kind not recognized for {card_number:?}");
                continue;
            };

            let (holo_power, name, text) = match oshi_skill.text().collect_vec()[..] {
                [holo_power, name, text] => (Some(holo_power), name, text),
                [name, text] => (None, name, text),
                _ => {
                    eprintln!("Oshi skill text format not recognized for {card_number:?}");
                    continue;
                }
            };
            let holo_power = holo_power.and_then(|holo_power| {
                holo_power
                    .trim_start_once("[ホロパワー：")
                    .trim_start_once("[ホロパワー:")
                    .trim_start_once("[holo Power：")
                    .trim_start_once("[holo Power:")
                    .trim_start_once("-")
                    .trim_end_once("]")
                    .trim_end_once("消費")
                    .to_string()
                    .try_into()
                    .ok()
            });

            let oshi_skill = OshiSkill {
                kind,
                holo_power,
                name: Localized::new(language, clean_text(name)),
                ability_text: Localized::new(language, fix_ability_text(text, language)),
            };
            oshi_skills.push(oshi_skill);
        }
        // replace existing skills. will need to import english skills later
        fix_oshi_skills(card, &mut oshi_skills, language);
        update_oshi_skills(card, oshi_skills, language, true, true);
    }

    fn update_card_keywords(
        card: &mut Card,
        hololive_card: &scraper::ElementRef,
        language: Language,
    ) {
        static KEYWORD: LazyLock<Selector> =
            LazyLock::new(|| Selector::parse(".keyword p:nth-child(2)").unwrap());

        let card_number = &card.card_number;
        // Keywords
        let mut keywords = vec![];
        for keyword in hololive_card.select(&KEYWORD) {
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
                name: Localized::new(language, clean_text(name)),
                ability_text: Localized::new(language, fix_ability_text(&text, language)),
            };
            keywords.push(keyword);
        }
        // replace existing keywords. will need to import english keywords later
        fix_keywords(card, &mut keywords);
        update_keywords(card, keywords, language, true, true);
    }

    fn update_card_arts(card: &mut Card, hololive_card: &scraper::ElementRef, language: Language) {
        static ART: LazyLock<Selector> =
            LazyLock::new(|| Selector::parse(".sp.arts p:nth-child(2)").unwrap());

        let card_number = &card.card_number;

        // Arts
        let mut arts = vec![];
        for art in hololive_card.select(&ART) {
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
            let mut art_text = art_text.lines().filter(|l| {
                let c = colors_from_str(l.trim());
                if c.is_empty() || l.chars().count() > 1 {
                    true
                } else {
                    cheers.extend(c);
                    false
                }
            });

            let mut name = String::new();
            for l in art_text.by_ref() {
                name.push_str(l);
                // the damage is the last part of the name text
                if l.contains('　') {
                    break;
                }
            }
            if name.is_empty() {
                eprintln!("Art name not found for {card_number:?}");
                continue;
            }

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

            let (name, power) = name.rsplit_once('　').unwrap_or((&name, ""));

            let art = Art {
                cheers,
                name: Localized::new(language, clean_text(name)),
                power: power.to_string().try_into().unwrap_or_default(),
                advantage,
                ability_text: text
                    .map(|text| Localized::new(language, fix_ability_text(&text, language))),
            };
            arts.push(art);
        }
        // replace existing arts. will need to import english arts later
        fix_arts(card, &mut arts, language);
        update_arts(card, arts, language, true, true);
    }

    fn update_card_illustrations(
        card: &mut Card,
        manage_id: u32,
        images_path: &Path,
        optimized_original_images: bool,
        hololive_card: &scraper::ElementRef,
        language: Language,
    ) {
        static INFO: LazyLock<Selector> =
            LazyLock::new(|| Selector::parse(".info dl :is(dt, dd)").unwrap());

        // Rarity
        let rarity = hololive_card
            .select(&INFO)
            .tuples()
            .map(|(key, value)| {
                let key = key.text().collect::<String>();
                let value = value.text().collect::<String>();
                let value = value.trim();
                (key.to_lowercase(), value.to_owned())
            })
            .find(|(key, _)| key == "レアリティ" || key == "rarity")
            .map(|(_, value)| value.to_string())
            .unwrap_or_else(|| "?".to_string());

        static IMAGE: LazyLock<Selector> = LazyLock::new(|| Selector::parse(".img img").unwrap());

        // Image path
        let mut card_img_path = hololive_card
            .select(&IMAGE)
            .next()
            .and_then(|c| c.value().attr("src"))
            .map(|s| {
                s.trim_start_matches("/wp-content/images/cardlist/")
                    .to_string()
            })
            .unwrap_or_default();
        card_img_path = fix_image_path(manage_id, language, &card_img_path);

        if !optimized_original_images {
            card_img_path = card_img_path.replace(".png", ".webp");
        }

        let illustrations = &mut card.illustrations;
        // find the card, first by manage_id, then by image, otherwise just add
        if let Some(illust) = {
            if let Some(i) = illustrations.iter_mut().find(|i| {
                i.manage_id
                    .value(language)
                    .iter()
                    .flatten()
                    .any(|m| *m == manage_id)
            }) {
                Some(i)
            } else {
                illustrations
                    .iter_mut()
                    .find(|i| i.img_path.value(language).as_ref() == Some(&card_img_path))
            }
        } {
            // only these fields are retrieved
            illust.card_number = card.card_number.clone();
            illust
                .manage_id
                .value_mut(language)
                .get_or_insert_default()
                .insert(manage_id);
            illust.rarity = rarity.into();
            // rename the old file
            if let Some(img_path) = illust.img_path.value_mut(language).as_mut()
                && card_img_path != *img_path
            {
                let old_path = images_path.join(&img_path);
                *img_path = card_img_path;
                let new_path = images_path.join(&img_path);
                if fs::exists(&old_path).unwrap_or_default() {
                    fs::create_dir_all(new_path.parent().unwrap()).unwrap();
                    fs::copy(old_path, new_path).unwrap();
                }
                *illust.img_last_modified.value_mut(language) = None;
            } else {
                *illust.img_path.value_mut(language) = Some(card_img_path);
            }
        } else {
            let illust = CardIllustration {
                card_number: card.card_number.clone(),
                manage_id: match language {
                    Language::Japanese => Localized {
                        japanese: Some([manage_id].into()),
                        ..Default::default()
                    },
                    Language::English => Localized {
                        english: Some([manage_id].into()),
                        ..Default::default()
                    },
                },
                rarity: rarity.into(),
                img_path: Localized::new(language, card_img_path),
                ..Default::default()
            };
            // add the card to the list
            illustrations.push(illust);
        }

        // sort the list, by oldest to latest
        illustrations.sort_by_cached_key(|c| c.manage_id.clone());
    }

    /// Product names listed in the card list entry (JP "収録商品", EN "Card Set"), one per line.
    /// `None` when the entry doesn't have the field.
    fn list_products(hololive_card: &scraper::ElementRef) -> Option<Vec<String>> {
        static INFO: LazyLock<Selector> =
            LazyLock::new(|| Selector::parse(".info dl :is(dt, dd)").unwrap());

        let (_, value) = hololive_card.select(&INFO).tuples().find(|(key, _)| {
            let key = key.text().collect::<String>().trim().to_lowercase();
            key == "収録商品" || key == "card set"
        })?;

        // one product per line, lines are separated by <br>
        let mut products = vec![String::new()];
        for child in value.children() {
            match child.value() {
                Node::Text(text) => products.last_mut().unwrap().push_str(text),
                Node::Element(e) if e.name() == "br" => products.push(String::new()),
                Node::Element(_) => {
                    if let Some(e) = scraper::ElementRef::wrap(child) {
                        products
                            .last_mut()
                            .unwrap()
                            .push_str(&e.text().collect::<String>());
                    }
                }
                _ => {}
            }
        }
        Some(
            products
                .iter()
                .map(|p| clean_product_name(p))
                .filter(|p| !p.is_empty())
                .unique()
                .collect(),
        )
    }

    /// Collapses whitespace runs: the list and detail pages don't indent the names the same way.
    pub fn clean_product_name(name: &str) -> String {
        name.split_ascii_whitespace().join(" ")
    }

    /// The product entries of a card detail page (`page_url`): the "収録商品" / "Featured
    /// Products" section, in page order.
    pub fn detail_products(
        page_url: &Url,
        page_content: &str,
        language: Language,
    ) -> Vec<ProductEntry> {
        static PRODUCTS: LazyLock<Selector> =
            LazyLock::new(|| Selector::parse(".cardlist-Detail_Products .products").unwrap());
        static NAME: LazyLock<Selector> =
            LazyLock::new(|| Selector::parse(".products-ttl p").unwrap());
        static CATEGORY: LazyLock<Selector> =
            LazyLock::new(|| Selector::parse(".products-ttl .cat").unwrap());
        static INFO: LazyLock<Selector> =
            LazyLock::new(|| Selector::parse("dl :is(dt, dd)").unwrap());
        static LINKS: LazyLock<Selector> = LazyLock::new(|| Selector::parse("a[href]").unwrap());

        let document = Html::parse_document(page_content);
        document
            .select(&PRODUCTS)
            .filter_map(|product| {
                let name = product
                    .select(&NAME)
                    .next()
                    .map(|n| clean_product_name(&n.text().collect::<String>()))
                    .filter(|n| !n.is_empty())?;
                let release_date = product
                    .select(&INFO)
                    .tuples()
                    .find(|(key, _)| {
                        let key = key.text().collect::<String>().trim().to_lowercase();
                        key == "発売日" || key == "release date"
                    })
                    .and_then(|(_, value)| {
                        let value = value.text().collect::<String>();
                        let date = parse_release_date(&value, language);
                        if date.is_none() && !value.trim().is_empty() {
                            eprintln!("Warning: cannot parse release date {value:?} of {name:?}");
                        }
                        date
                    });
                // links: "CARD LIST" /cardlist/cardsearch/?expansion=hBP01, and "MORE" the
                // product page, when there is one
                let links = product
                    .select(&LINKS)
                    .filter_map(|a| page_url.join(a.value().attr("href")?).ok())
                    .collect_vec();
                let expansion = links.iter().find_map(|url| {
                    url.query_pairs()
                        .find(|(k, _)| k == "expansion")
                        .map(|(_, v)| v.trim().to_string())
                        .filter(|v| !v.is_empty())
                });
                let url = links
                    .iter()
                    .find(|url| !url.query_pairs().any(|(k, _)| k == "expansion"))
                    .map(|url| url.to_string());
                // the site's category: <span class="cat boosters">Booster</span>
                let category = product.select(&CATEGORY).next().and_then(|c| {
                    c.value()
                        .classes()
                        .find(|class| *class != "cat")
                        .map(str::to_owned)
                });
                Some(ProductEntry {
                    name,
                    expansion,
                    release_date,
                    category,
                    url,
                })
            })
            .collect()
    }

    /// The products of a page of the products pages of the official site (`/products/`): name
    /// and "MORE" page url, in page order.
    pub fn listing_products(page_url: &Url, page_content: &str) -> Vec<(String, String)> {
        static ITEMS: LazyLock<Selector> =
            LazyLock::new(|| Selector::parse(".product-list li").unwrap());
        static NAME: LazyLock<Selector> = LazyLock::new(|| Selector::parse(".ttl").unwrap());
        static LINK: LazyLock<Selector> =
            LazyLock::new(|| Selector::parse(".btn a[href]").unwrap());
        let document = Html::parse_document(page_content);
        document
            .select(&ITEMS)
            .filter_map(|item| {
                let name = item
                    .select(&NAME)
                    .next()
                    .map(|n| clean_product_name(&n.text().collect::<String>()))
                    .filter(|n| !n.is_empty())?;
                let url = page_url
                    .join(item.select(&LINK).next()?.value().attr("href")?)
                    .ok()?;
                Some((name, url.to_string()))
            })
            .collect()
    }

    /// All the products of the products pages of the official site, `None` if a page can't be
    /// read. The pages are /products/, /products/page/2/, ... until one without products.
    fn retrieve_listing_products(site: &str) -> Option<Vec<(String, String)>> {
        /// More pages than that: something is wrong.
        const MAX_PAGES: usize = 100;
        let mut listing = vec![];
        for page in 1..=MAX_PAGES {
            let url = if page == 1 {
                format!("{site}products/")
            } else {
                format!("{site}products/page/{page}/")
            };
            let content = match http_client()
                .get(&url)
                .send()
                .and_then(|resp| resp.error_for_status())
                .and_then(|resp| resp.text())
            {
                Ok(content) => content,
                // past the last page
                Err(error)
                    if error.status() == Some(reqwest::StatusCode::NOT_FOUND) && page > 1 =>
                {
                    return Some(listing);
                }
                Err(error) => {
                    eprintln!("Warning: cannot retrieve {url}: {error}");
                    return None;
                }
            };
            let products = listing_products(&Url::parse(&url).ok()?, &content);
            if products.is_empty() {
                return Some(listing);
            }
            listing.extend(products);
        }
        eprintln!("Warning: more than {MAX_PAGES} products pages at {site}products/");
        None
    }

    /// Whether a page is not found (`None` if it can't be checked).
    fn page_not_found(url: &str) -> Option<bool> {
        let status = http_client().get(url).send().ok()?.status();
        Some(status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::GONE)
    }

    /// Parses "2026年09月19日(土)" (JP) or "July 11, 2025" (EN) into "2026-09-19" / "2025-07-11".
    pub fn parse_release_date(date: &str, language: Language) -> Option<String> {
        let date = date.trim();
        let (year, month, day) = match language {
            Language::Japanese => {
                let numbers: Vec<u32> = date
                    .split(|c: char| !c.is_ascii_digit())
                    .filter(|n| !n.is_empty())
                    .take(3)
                    .map(|n| n.parse().ok())
                    .collect::<Option<_>>()?;
                let [year, month, day] = numbers[..] else {
                    return None;
                };
                (year, month, day)
            }
            Language::English => {
                const MONTHS: [&str; 12] = [
                    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov",
                    "dec",
                ];
                let mut parts = date
                    .split(|c: char| c.is_whitespace() || c == ',' || c == '.')
                    .filter(|p| !p.is_empty());
                let month = parts.next()?.to_lowercase();
                let month = MONTHS.iter().position(|m| month.starts_with(m))? as u32 + 1;
                let day = parts.next()?.parse().ok()?;
                let year = parts.next()?.parse().ok()?;
                (year, month, day)
            }
        };
        ((2000..3000).contains(&year) && (1..=12).contains(&month) && (1..=31).contains(&day))
            .then(|| format!("{year:04}-{month:02}-{day:02}"))
    }

    fn update_card_illustrations_more(
        card: &mut Card,
        illust_idx: usize,
        page_content: &str,
        language: Language,
    ) -> bool {
        static EXTRA: LazyLock<Selector> =
            LazyLock::new(|| Selector::parse(".extra p:nth-child(2)").unwrap());

        static ILLUSTRATOR: LazyLock<Selector> =
            LazyLock::new(|| Selector::parse(".ill-name span").unwrap());

        let mut updated = false;

        // parse the content and update cards
        let document = Html::parse_document(page_content);

        // Extra
        let mut extra = document
            .select(&EXTRA)
            .next()
            .map(|c| c.text().collect::<String>())
            .map(|c| clean_text(&c))
            .map(|c| Localized::new(language, c));
        fix_extra(card, &mut extra, language);
        if extra.as_ref().map(|e| e.value(language))
            != card.extra.as_ref().map(|e| e.value(language))
        {
            updated = true;
        }
        update_extra(card, extra, language, true, true);

        // get the illustrator from the card
        let illustrator = document
            .select(&ILLUSTRATOR)
            .next()
            .map(|c| c.text().collect::<String>())
            .unwrap_or_default();

        // not a valid illustrator name
        if !illustrator.contains("ノーマル") && !illustrator.contains("パラレル") {
            if Some(&illustrator) != card.illustrations[illust_idx].illustrator.as_ref() {
                updated = true;
            }
            // save empty string to indicate that we know there is no illustrator. no need for request
            if !illustrator.is_empty() || card.illustrations[illust_idx].illustrator.is_none() {
                card.illustrations[illust_idx].illustrator = Some(illustrator);
            }
        }

        updated
    }

    // --- Fixes for known issues in the official database ---
    fn fix_card_number(manage_id: u32, card_number: &mut String, language: Language) {
        if language == Language::Japanese {
            // hSD06-001 Kazama Iroha oshi card number (the official db has a bug, it's the second time it happened)
            if manage_id == 532 {
                *card_number = "hSD06-001".into();
            }
        }
    }

    fn fix_buzz(card: &Card, buzz: &mut bool) {
        // hBP07-019 Vestia Zeta is Buzz
        if card.card_number == "hBP07-019" {
            *buzz = true;
        }

        // hBP07-048 Elizabeth Rose Bloodflame is Buzz
        if card.card_number == "hBP07-048" {
            *buzz = true;
        }

        // hBP07-076 Nerissa Ravencroft is Buzz
        if card.card_number == "hBP07-076" {
            *buzz = true;
        }
    }

    fn fix_tags(card: &Card, tags: &mut Vec<Localized<String>>, language: Language) {
        if language == Language::Japanese {
            // hBP06-087 Shimeji Dance missing tag
            if card.card_number == "hBP06-087" {
                *tags = vec![Localized::jp("#きのこ".into())];
            }

            // hBP06-097 Cute Varsity Jacket missing tag
            if card.card_number == "hBP06-097" {
                *tags = vec![Localized::jp("#Buzzグッズ".into())];
            }
        }

        if language == Language::English {
            // #ID Gen N have wrong format
            tags.iter_mut().for_each(|t| match t.english.as_deref() {
                Some("#IDGen 1") => *t = Localized::en("#ID Gen 1".into()),
                Some("#IDGen 2") => *t = Localized::en("#ID Gen 2".into()),
                Some("#IDGen 3") => *t = Localized::en("#ID Gen 3".into()),
                _ => {}
            });
        }
    }

    fn fix_colors(card: &Card, colors: &mut Vec<Color>) {
        // hSD01-002 AZKi oshi card color (the official EN db has a bug)
        if card.card_number == "hSD01-002" {
            *colors = vec![Color::Green];
        }
    }

    fn fix_bloom_level(card: &Card, bloom_level: &mut Option<BloomLevel>) {
        // hSD10-010 Isaki Riona bloom level mismatch
        if card.card_number == "hSD10-010" {
            *bloom_level = Some(BloomLevel::Spot);
        }
    }

    #[must_use]
    fn fix_ability_text(text: &str, language: Language) -> String {
        let mut text = clean_text(text);

        // hBP05-006, hBP05-017, hBP04-091, hBP05-075 have "◇" instead of the text "colorless cheer"
        if language == Language::Japanese {
            text = text.replace('◇', "無色エール").trim().to_string();
        } else {
            text = text.replace('◇', "colorless").trim().to_string();
        }

        if language == Language::English {
            // hBP02-079, hBP02-041, hBP02-017 have extra "▲" in ability text, in English
            text = text.replace('▲', "").trim().to_string();

            // #ID Gen N have wrong format
            text = text.replace("#IDGen 1", "#ID Gen 1");
            text = text.replace("#IDGen 2", "#ID Gen 2");
            text = text.replace("#IDGen 3", "#ID Gen 3");
        }

        text
    }

    fn fix_oshi_skills(card: &Card, oshi_skills: &mut [OshiSkill], language: Language) {
        // fix English oshi skills with known issues
        if language == Language::English {
            // fix hBP03-004 FUWAMOCO, oshi skill has wrong holoPower
            if card.card_number == "hBP03-004"
                && let Some(skill) = oshi_skills
                    .iter_mut()
                    .find(|a| a.name.english.as_deref() == Some("BAU BAU!"))
            {
                skill.holo_power = Some(HoloPower::Basic(2));
            }
        }
    }

    fn fix_keywords(_card: &Card, _keywords: &mut Vec<Keyword>) {}

    fn fix_arts(card: &Card, arts: &mut Vec<Art>, language: Language) {
        // fix Japanese arts with known issues
        if language == Language::Japanese {
            // hBP05-061 Nerissa Ravencroft's art Unleashed Charm should be 120+
            if card.card_number == "hBP05-061"
                && let Some(art) = arts
                    .iter_mut()
                    .find(|a| a.name.japanese.as_deref() == Some("Unleashed Charm"))
            {
                art.power = ArtPower::Plus(120);
            }
        }

        // fix English arts with known issues
        if language == Language::English {
            // fix hSD01-006 Tokino Sora, art has extra new line
            if card.card_number == "hSD01-006"
                && let Some(art) = arts
                    .iter_mut()
                    .find(|a| a.name.english.as_deref() == Some("SorAZ Sympathy"))
            {
                art.ability_text = Some(Localized::en(
                    "If an 〈AZKi〉 holomem is on your stage, Arts +50.".into(),
                ));
            }

            // fix hSD02-008 Nakiri Ayame, art is missing text
            if card.card_number == "hSD02-008"
                && let Some(art) = arts.iter_mut().find(|a| {
                    a.name.english.as_deref() == Some("I Wonder what's Inside the Present?")
                })
            {
                art.ability_text = Some(Localized::en(
                    "You may archive 1 card from your hand:Deal 50 special damage to your opponent's center holomem or collab holomem."
                        .into(),
                ));
            }

            // fix hSD02-010 Shirakami Fubuki, art has extra text
            if card.card_number == "hSD02-010"
                && let Some(art) = arts
                    .iter_mut()
                    .find(|a| a.name.english.as_deref() == Some(r#"AyaFubuMi's "Fubu""#))
            {
                art.ability_text = None;
            }

            // fix hSD03-009 Nekomata Okayu, completely missing arts
            if card.card_number == "hSD03-009" {
                *arts = vec![
                    Art {
                        cheers: vec![Color::Blue, Color::Colorless],
                        name: Localized::en("Nom Nom".into()),
                        power: ArtPower::Basic(60),
                        advantage: Some((Color::White, 50)),
                        ability_text: None,
                    },
                    Art {
                        cheers: vec![Color::Blue, Color::Blue, Color::Colorless, Color::Colorless],
                        name: Localized::en("Okayu~".into()),
                        power: ArtPower::Basic(100),
                        advantage: Some((Color::White, 50)),
                        ability_text: Some(Localized::en("You may archive 2 blue cheers from this holomem:Deal 30 special damage to your opponent's center holomem and 1 of their back holomem.".into())),
                    },
                ];
            }

            // fix hSD04-009 Yuzuki Choco, completely missing arts
            if card.card_number == "hSD04-009" {
                *arts = vec![
                    Art {
                        cheers: vec![Color::Purple, Color::Colorless],
                        name: Localized::en("33… 22… 11…".into()),
                        power: ArtPower::Basic(50),
                        advantage: Some((Color::Green, 50)),
                        ability_text: None,
                    },
                    Art {
                        cheers: vec![Color::Purple, Color::Purple, Color::Colorless],
                        name: Localized::en("Act-".into()),
                        power: ArtPower::Plus(60),
                        advantage: Some((Color::Green, 50)),
                        ability_text: Some(Localized::en(
                            "This Arts gets +40 for each event you used this turn.".into(),
                        )),
                    },
                ];
            }
        }
    }

    fn fix_extra(card: &Card, extra: &mut Option<Extra>, language: Language) {
        // hSD08-002 Amane Kanata does not have extra text
        if card.card_number == "hSD08-002" {
            *extra = None;
        }

        // hBP05-082 Aki Rosenthal's Axe does have extra text
        if card.card_number == "hBP05-082" {
            if language == Language::Japanese {
                *extra = Some(Localized::jp("このツールは〈石の斧〉としても扱う".into()));
            }
            if language == Language::English {
                *extra = Some(Localized::en(
                    "This tool is also regarded as 〈Stone Axe〉".into(),
                ));
            }
        }
    }

    fn fix_max_amount(card: &mut Card, language: Language) {
        // fix missing max amount
        if card.max_amount.value(language).is_none() {
            *card.max_amount.value_mut(language) = Some(match card.card_type {
                CardType::OshiHoloMember => 1,
                CardType::Cheer => 20,
                _ => 4,
            });
        }

        // fix card amount for unlimited debuts, overwrite default amount
        if card.max_amount.value(language).is_some_and(|v| v == 4)
            && card.extra.as_ref().is_some_and(|e| {
                e.japanese.as_deref() == Some("このホロメンはデッキに何枚でも入れられる")
                    || e.english.as_deref()
                        == Some("You may include any number of this holomem in the deck")
            })
        {
            *card.max_amount.value_mut(language) = Some(50);
        }
    }

    fn fix_image_path(manage_id: u32, language: Language, image_path: &str) -> String {
        // fix issues with Japanese site
        if language == Language::Japanese {
            // fix Nerissa Ravencroft oshi card HR image path
            if manage_id == 1373 {
                return "hBP05/hBP02-065_HR.png".into();
            }
        }

        // fix issues with English site
        if language == Language::English {
            // fix Mikkorone 24 support SEC image path
            if manage_id == 419 {
                return "hBP02/EN_hBP02-084_SEC.png".into();
            }
        }

        image_path.to_string()
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
    /// After that many card list pages that can't be read, the site is considered down.
    const MAX_FAILED_LIST_PAGES: usize = 10;

    /// A page of the card list of the official site.
    #[derive(Debug, PartialEq, Eq)]
    pub enum ListPage {
        Cards(String),
        /// After the last page.
        End,
        /// The page couldn't be read (network error, error status).
        Failed(String),
    }

    /// Retrieves a page of the card list, trying again once if it fails. The site answers the
    /// pages after the last one with "Page not found" (and a 404 status on the english site).
    pub fn fetch_list_page(url: &str, referrer: &str, query: &[(&str, &str)]) -> ListPage {
        let mut error = String::new();
        for _ in 0..2 {
            let response = http_client()
                .get(url)
                .query(query)
                .header(REFERER, referrer)
                .send()
                .and_then(|resp| {
                    let status = resp.status();
                    resp.text().map(|content| (status, content))
                });
            match response {
                Ok((status, content)) => {
                    // no more card in this page
                    if content.contains(
                        "<title>hololive OFFICIAL CARD GAME｜ホロライブプロダクション</title>",
                    ) || content
                        .contains("<title>hololive OFFICIAL CARD GAME｜hololive production</title>")
                        || content.contains("Page not found")
                    {
                        return ListPage::End;
                    }
                    if status.is_success() {
                        return ListPage::Cards(content);
                    }
                    error = format!("HTTP {status}");
                }
                Err(err) => error = err.to_string(),
            }
        }
        ListPage::Failed(error)
    }

    /// Whether the cards of the read pages can be updated: when most pages failed, or the site
    /// went down while reading (`stopped`), the read pages may not be what they seem and all the
    /// data is kept.
    pub fn list_pages_usable(read: usize, failed: usize, stopped: bool) -> bool {
        !stopped && failed <= read
    }

    pub fn retrieve_card_info_from_hololive(
        all_cards: &mut CardsDatabase,
        products: &mut ProductsDatabase,
        number_filter: &Option<String>,
        expansion: &Option<String>,
        optimized_original_images: bool,
        // the images folder of the language
        images_path: &Path,
        language: Language,
    ) -> Vec<(String, usize)> {
        if number_filter.is_none() && expansion.is_none() {
            println!(
                "Retrieve ALL cards info from Hololive official site for language: {language:?}"
            );
        } else {
            println!(
                "Retrieve cards info from Hololive official site with filters - language: {:?}, number: {}, expension: {}",
                language,
                number_filter.as_deref().unwrap_or("all"),
                expansion.as_deref().unwrap_or("all")
            );
        }

        let mut filtered_cards = HashSet::new();
        let mut listed_products: BTreeMap<u32, Vec<String>> = BTreeMap::new();
        let mut updated_count = 0;

        // Download pages in parallel. A page that can't be read is skipped (its cards keep their
        // data); after too many, the site is down: stop
        let failed_count = AtomicUsize::new(0);
        let pages: HashMap<usize, Option<(String, String)>> = (1..)
            .par_bridge()
            .map({
                let failed_count = &failed_count;
                move |page| {
                    if failed_count.load(Ordering::Relaxed) >= MAX_FAILED_LIST_PAGES {
                        return None;
                    }
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

                    let page_number = page.to_string();
                    let query = [
                        ("view", "text"),
                        ("sort", "old"),
                        ("keyword", number_filter.as_deref().unwrap_or_default()),
                        ("expansion_name", expansion.as_deref().unwrap_or_default()),
                        ("page", page_number.as_str()),
                    ];
                    match fetch_list_page(url, referrer, &query) {
                        ListPage::Cards(content) => {
                            println!("Page {page} retrieved");
                            Some((page, Some((url.to_owned(), content))))
                        }
                        ListPage::End => {
                            println!("Page {page} not found, stop retrieving more pages");
                            None
                        }
                        ListPage::Failed(error) => {
                            eprintln!("Warning: card list page {page} skipped: {error}");
                            failed_count.fetch_add(1, Ordering::Relaxed);
                            Some((page, None))
                        }
                    }
                }
            })
            .while_some()
            .collect();

        let read_pages = pages.values().filter(|p| p.is_some()).count();
        let failed_pages = pages.len() - read_pages;
        if !list_pages_usable(
            read_pages,
            failed_pages,
            failed_count.load(Ordering::Relaxed) >= MAX_FAILED_LIST_PAGES,
        ) {
            warn_skipped(&format!(
                "the card list of the official site couldn't be read ({failed_pages} pages failed, {read_pages} read), keeping all the {language:?} data"
            ));
            return vec![];
        }
        if failed_pages > 0 {
            warn_skipped(&format!(
                "{failed_pages} {language:?} card list pages of the official site skipped, their cards keep their data"
            ));
        }

        // Process pages sequentially
        'pages: for page in pages.keys().copied().sorted() {
            let Some(Some((url, content))) = pages.get(&page) else {
                continue;
            };

            // parse the content and update cards
            let document = Html::parse_document(content);
            static CARDS: LazyLock<Selector> = LazyLock::new(|| Selector::parse("li a").unwrap());
            static CARD_NUMBER: LazyLock<Selector> =
                LazyLock::new(|| Selector::parse(".number").unwrap());

            let mut page_updated_count = 0;

            'cards: for hololive_card in document.select(&CARDS) {
                let Some(mut card_number) = hololive_card
                    .select(&CARD_NUMBER)
                    .next()
                    .map(|c| c.text().collect::<String>())
                else {
                    println!("Card number not found");
                    continue 'pages;
                };

                // if the card number is empty, skip it
                if card_number.is_empty() || card_number == "null" {
                    continue 'cards;
                }

                // need to go to the detailed page
                let Some(card_url) = hololive_card.attr("href") else {
                    eprintln!("Card url not found for {card_number:?}");
                    continue 'cards;
                };
                let card_url = Url::parse(url).unwrap().join(card_url).unwrap();
                let Some(manage_id) = card_url
                    .query_pairs()
                    .into_owned()
                    .find(|(k, _)| k == "id")
                    .and_then(|(_, v)| v.parse().ok())
                else {
                    eprintln!("Card id not found for {card_number:?}");
                    continue 'cards;
                };

                // fix card number if needed
                fix_card_number(manage_id, &mut card_number, language);

                // remove the old manage_id, if it exists
                all_cards
                    .values_mut()
                    .flat_map(|cs| cs.illustrations.iter_mut())
                    .filter(|c| {
                        c.card_number != card_number
                            && c.manage_id
                                .value(language)
                                .iter()
                                .flatten()
                                .any(|m| Some(m) == Some(&manage_id))
                    })
                    .for_each(|c| {
                        if let Some(c_manage_id) = c.manage_id.value_mut(language)
                            && let Some(id) = Some(manage_id)
                        {
                            c_manage_id.retain(|&m| m != id);
                        }
                        // remove empty manage_id
                        c.manage_id.value_mut(language).take_if(|v| v.is_empty());
                    });

                let card = all_cards.entry(card_number.clone()).or_default();

                card.card_number = card_number;
                update_card_info(card, &hololive_card, language);
                update_card_oshi_skills(card, &hololive_card, language);
                update_card_keywords(card, &hololive_card, language);
                update_card_arts(card, &hololive_card, language);
                update_card_illustrations(
                    card,
                    manage_id,
                    images_path,
                    optimized_original_images,
                    &hololive_card,
                    language,
                );

                // product names, the release dates are on the detail page
                if let Some(products) = list_products(&hololive_card) {
                    listed_products.insert(manage_id, products);
                }

                // add to filtered cards
                filtered_cards.insert(manage_id);

                page_updated_count += 1;
            }

            // Increment the total updated count
            updated_count += page_updated_count;

            println!("Page {page} done: updated {page_updated_count} cards");
        }
        println!("Updated {} cards", updated_count);

        // Products: the card list only has their names, the details (expansion code, release
        // date, ...) are on the card detail pages. They are the same for all the cards of a
        // product, so only one detail page is read per name that is not in the catalog, or that
        // has no release date yet. Card pools (not products) are never in the catalog, they are
        // checked on each run too.
        let names = catalog_names(products, language);
        let mut product_pages_ids = BTreeSet::new();
        let mut checked_names = BTreeSet::new();
        for (manage_id, listed_names) in &listed_products {
            for name in listed_names {
                if needs_details(products, &names, name, language)
                    && checked_names.insert(name.clone())
                {
                    product_pages_ids.insert(*manage_id);
                }
            }
        }
        println!(
            "Products: {} listed, retrieving {} card pages for {} names: {}",
            listed_products.values().flatten().unique().count(),
            product_pages_ids.len(),
            checked_names.len(),
            checked_names.iter().join(", ")
        );
        let product_pages: HashMap<String, String> = product_pages_ids
            .iter()
            .par_bridge()
            .filter_map(|manage_id| {
                let illust = CardIllustration {
                    manage_id: Localized::new(language, [*manage_id].into()),
                    ..Default::default()
                };
                let url = illust.official_site_urls(language).pop()?;
                let content = http_client()
                    .get(&url)
                    .send()
                    .and_then(|resp| resp.error_for_status())
                    .and_then(|resp| resp.text());
                match content {
                    Ok(content) => Some((url, content)),
                    Err(error) => {
                        eprintln!("Warning: cannot retrieve {url}: {error}");
                        None
                    }
                }
            })
            .collect();
        // sorted, for a deterministic catalog
        let entries = product_pages
            .iter()
            .sorted()
            .filter_map(|(url, content)| Some((Url::parse(url).ok()?, content)))
            .flat_map(|(url, content)| detail_products(&url, content, language))
            .collect_vec();
        let mut resolved: HashMap<String, ListedProduct> = names
            .into_iter()
            .map(|(name, code)| (name, ListedProduct::Product(code)))
            .collect();
        resolved.extend(update_catalog(products, &entries, language));
        // the product pages: some "MORE" links of the card pages are dead
        let site = match language {
            Language::Japanese => "https://hololive-official-cardgame.com/",
            Language::English => "https://en.hololive-official-cardgame.com/",
        };
        match retrieve_listing_products(site) {
            Some(listing) => {
                let fixed_count = fix_product_urls(products, &listing, language, page_not_found);
                println!(
                    "Products pages: {} products listed, {fixed_count} product pages fixed",
                    listing.len()
                );
            }
            None => eprintln!(
                "Warning: the products pages of the official site couldn't be read, the product pages are kept"
            ),
        }
        update_short_names(products);
        let (products_updated_count, unknown_names) =
            update_printings(all_cards, &listed_products, &resolved, language);
        for name in &unknown_names {
            eprintln!("Warning: product {name:?} is unknown, the stored products are kept");
        }
        println!("Updated products of {products_updated_count} illustrations");

        // Download illustration specific info in parallel
        let pages: HashMap<String, Option<String>> = all_cards
            .values()
            .flat_map(|card| card.illustrations.iter().map(move |illust| (card, illust)))
            .filter(|(_, illust)| {
                illust
                    .manage_id
                    .value(language)
                    .iter()
                    .flatten()
                    .any(|m| filtered_cards.contains(m))
            })
            .enumerate()
            .par_bridge()
            .filter_map({
                |(index, (card, illust))| {
                    let card_urls = illust.official_site_urls(language);
                    let url = card_urls.last()?;
                    let mut need_page = false;

                    // Missing illustrator
                    need_page |= illust.illustrator.is_none();

                    // Support card with extra
                    need_page |=
                        matches!(card.card_type, CardType::Support(_)) && card.extra.is_some();

                    // Skip if no need to retrieve the page
                    if !need_page {
                        return Some((url.to_owned(), None));
                    }

                    // Already retrieved for the products
                    if let Some(content) = product_pages.get(url) {
                        return Some((url.to_owned(), Some(content.clone())));
                    }

                    // a page that can't be read is skipped, the illustration keeps its data
                    let content = match http_client()
                        .get(url)
                        .send()
                        .and_then(|resp| resp.error_for_status())
                        .and_then(|resp| resp.text())
                    {
                        Ok(content) => content,
                        Err(error) => {
                            eprintln!("Warning: cannot retrieve {url}: {error}");
                            return None;
                        }
                    };

                    let index = index + 1;
                    if index.is_multiple_of(100) {
                        println!("Illustration {index} retrieved");
                    }

                    Some((url.to_owned(), Some(content)))
                }
            })
            .collect();

        // Process illustrations sequentially
        let mut illust_updated_count: u32 = 0;
        let mut illust_num = 0;
        for card in all_cards.values_mut() {
            for illust_idx in card
                .illustrations
                .iter()
                .enumerate()
                .filter(|(_, illust)| {
                    illust
                        .manage_id
                        .value(language)
                        .iter()
                        .flatten()
                        .any(|m| filtered_cards.contains(m))
                })
                .map(|(idx, _)| idx)
                .collect_vec()
            {
                illust_num += 1;

                let card_urls = card.illustrations[illust_idx].official_site_urls(language);
                let Some(url) = card_urls.last() else {
                    continue;
                };

                // Retrieve the page content for the illustration, warn if needed and not found
                let content = match pages.get(url) {
                    Some(Some(content)) => content,
                    Some(_) => continue,
                    _ => {
                        eprintln!("Page {url} not found in retrieved pages");
                        continue;
                    }
                };

                if update_card_illustrations_more(card, illust_idx, content, language) {
                    illust_updated_count += 1;
                    if illust_updated_count.is_multiple_of(100) {
                        println!(
                            "Illustration {illust_num} done: updated {illust_updated_count} illustrations"
                        );
                    }
                }
            }
        }
        println!("Updated {} illustrations", illust_updated_count);

        all_cards
            .values()
            .flat_map(|cs| cs.illustrations.iter().enumerate())
            .filter(|c| {
                c.1.manage_id
                    .value(language)
                    .iter()
                    .flatten()
                    .any(|m| filtered_cards.contains(m))
            })
            .map(|c| (c.1.card_number.clone(), c.0))
            .collect()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        // from https://en.hololive-official-cardgame.com/cardlist/cardsearch_ex?keyword=hBP01-021
        const EN_LIST: &str = r#"<ul><li class="ex-item"><a href="/cardlist/?id=3">
<p class="number">hBP01-021</p>
<div class="info">
<dl>
    <dt>Card Type</dt>
    <dd>holomem</dd>
            <dt>Rarity</dt>
    <dd>C</dd>
    	    <dt>Card Set</dt>
    <dd>
    		Start Deck – Tokino Sora & AZKi<br />Booster Pack – Blooming Radiance<br />Selection Cup - Diva Fever     </dd>
	        </dl>
</div></a></li></ul>"#;

        // from https://en.hololive-official-cardgame.com/cardlist/?id=3
        const EN_DETAIL: &str = r#"<div class="cardlist-Detail_Products">
<h2 class="txtarea-Ttl Serif ja bold">Featured Products</h2>
<div class="products">
<div class="products-ttl"><span class="cat boosters">Booster</span><p>Booster Pack – Blooming Radiance</p></div>
<dl>
<dt>Release Date</dt>
<dd>July 11, 2025</dd>
</dl>
<div class="btn">
<a class="cardlist-Detail_Products_Btn Serif hover" href="https://en.hololive-official-cardgame.com/products/post/booster-pack-blooming-radiance/">MORE</a>
<a class="cardlist-Detail_Products_Btn Serif hover" href="/cardlist/cardsearch/?expansion=hBP01">CARD LIST</a>
</div>
</div>
<div class="products">
<div class="products-ttl"><span class="cat decks">Deck</span><p>Start Deck – Tokino Sora & AZKi</p></div>
<dl>
<dt>Release Date</dt>
<dd>July 11, 2025</dd>
</dl>
<div class="btn">
<a class="cardlist-Detail_Products_Btn Serif hover" href="/cardlist/cardsearch/?expansion=hSD01">CARD LIST</a>
</div>
</div>
<div class="products">
<div class="products-ttl"><span class="cat accessories">Accessory</span><p>Selection Cup - Diva Fever </p></div>
<dl>
<dt>Release Date</dt>
<dd>November 20, 2026</dd>
</dl>
<div class="btn">
<a class="cardlist-Detail_Products_Btn Serif hover" href="https://en.hololive-official-cardgame.com/events_detail/diva-fever_selection-cup/">MORE</a>
<a class="cardlist-Detail_Products_Btn Serif hover" href="/cardlist/cardsearch/?expansion=sele07">CARD LIST</a>
</div>
</div>
</div>"#;

        // from https://hololive-official-cardgame.com/cardlist/?id=2991
        const JP_PR_DETAIL: &str = r#"<div class="cardlist-Detail_Products">
<h2 class="txtarea-Ttl Serif ja bold">収録商品</h2>
<div class="products">
<div class="products-ttl"><span class="cat pr">PRカード</span><p>PRカード</p></div>
<div class="btn">
<a class="cardlist-Detail_Products_Btn Serif hover" href="/cardlist/cardsearch/?expansion=hPR">CARD LIST</a>
</div>
</div>
</div>"#;

        fn product(
            name: &str,
            date: Option<&str>,
            expansion: &str,
            category: &str,
            url: Option<&str>,
        ) -> ProductEntry {
            ProductEntry {
                name: name.into(),
                expansion: Some(expansion.into()),
                release_date: date.map(Into::into),
                category: Some(category.into()),
                url: url.map(Into::into),
            }
        }

        fn en_page() -> Url {
            Url::parse("https://en.hololive-official-cardgame.com/cardlist/?id=3").unwrap()
        }

        fn jp_page() -> Url {
            Url::parse("https://hololive-official-cardgame.com/cardlist/?id=2991").unwrap()
        }

        #[test]
        fn test_list_products() {
            let document = Html::parse_document(EN_LIST);
            let card = document
                .select(&Selector::parse("li a").unwrap())
                .next()
                .unwrap();
            assert_eq!(
                list_products(&card),
                Some(vec![
                    "Start Deck – Tokino Sora & AZKi".to_string(),
                    "Booster Pack – Blooming Radiance".to_string(),
                    "Selection Cup - Diva Fever".to_string(),
                ])
            );
        }

        #[test]
        fn test_detail_products() {
            assert_eq!(
                detail_products(&en_page(), EN_DETAIL, Language::English),
                vec![
                    product(
                        "Booster Pack – Blooming Radiance",
                        Some("2025-07-11"),
                        "hBP01",
                        "boosters",
                        Some(
                            "https://en.hololive-official-cardgame.com/products/post/booster-pack-blooming-radiance/"
                        ),
                    ),
                    product(
                        "Start Deck – Tokino Sora & AZKi",
                        Some("2025-07-11"),
                        "hSD01",
                        "decks",
                        None,
                    ),
                    product(
                        "Selection Cup - Diva Fever",
                        Some("2026-11-20"),
                        "sele07",
                        "accessories",
                        Some(
                            "https://en.hololive-official-cardgame.com/events_detail/diva-fever_selection-cup/"
                        ),
                    ),
                ]
            );
            assert_eq!(
                detail_products(&jp_page(), JP_PR_DETAIL, Language::Japanese),
                vec![product("PRカード", None, "hPR", "pr", None)]
            );
        }

        #[test]
        fn test_listing_products() {
            let page = r#"<div class="product-list"><ul>
<li ><div class="flex-wrap"><div class="image"><img src="/a.webp" alt=""></div>
<div class="detail"><div class="ttl-box"><div class="cate specials"><p>Specials</p></div>
<p class="ttl"><span></span>スタートデッキセット-2025
  ホロナツパラダイスver.-</p></div>
<div class="info"><dl><dt>発売日</dt><dd>イベント限定</dd></dl></div></div></div>
<div class="btn"><a href="https://hololive-official-cardgame.com/products/post/startdeckset-2025summer/">MORE</a></div></li>
<li ><div class="detail"><p class="ttl"><span></span>オフィシャルホロカバインダー</p></div>
<div class="btn"><a href="/products/post/binder/">MORE</a></div></li>
<li ><div class="detail"><p class="ttl"><span></span>No page</p></div></li>
</ul></div>"#;
            let url =
                Url::parse("https://hololive-official-cardgame.com/products/page/6/").unwrap();
            assert_eq!(
                listing_products(&url, page),
                [
                    (
                        "スタートデッキセット-2025 ホロナツパラダイスver.-",
                        "https://hololive-official-cardgame.com/products/post/startdeckset-2025summer/"
                    ),
                    (
                        "オフィシャルホロカバインダー",
                        "https://hololive-official-cardgame.com/products/post/binder/"
                    ),
                ]
                .map(|(name, url)| (name.to_string(), url.to_string()))
            );
            assert!(listing_products(&url, "<html><body>nothing</body></html>").is_empty());
        }

        #[test]
        fn test_fetch_list_page() {
            use crate::test_server::{http, serve};
            let query = [("page", "1")];
            // a page of cards, after a server error (tried again)
            let url = serve(vec![
                http("500 Internal Server Error", "oops"),
                http("200 OK", EN_LIST),
            ]);
            assert_eq!(
                fetch_list_page(&url, &url, &query),
                ListPage::Cards(EN_LIST.into())
            );
            // after the last page
            let url = serve(vec![http(
                "200 OK",
                "<title>hololive OFFICIAL CARD GAME｜hololive production</title>",
            )]);
            assert_eq!(fetch_list_page(&url, &url, &query), ListPage::End);
            // the english site: a 404 after the last page
            let url = serve(vec![http(
                "404 Not Found",
                "<html><body>Page not found</body></html>",
            )]);
            assert_eq!(fetch_list_page(&url, &url, &query), ListPage::End);
            // failing twice
            let url = serve(vec![
                http("503 Service Unavailable", ""),
                http("503 Service Unavailable", ""),
            ]);
            assert!(matches!(
                fetch_list_page(&url, &url, &query),
                ListPage::Failed(_)
            ));
            // nothing listening
            let url = {
                let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                format!("http://{}/", listener.local_addr().unwrap())
            };
            assert!(matches!(
                fetch_list_page(&url, &url, &query),
                ListPage::Failed(_)
            ));
        }

        #[test]
        fn test_list_pages_usable() {
            // a few pages skipped: the others are updated
            assert!(list_pages_usable(200, 3, false));
            assert!(list_pages_usable(10, 10, false));
            // most pages failed, or the site went down: keep everything
            assert!(!list_pages_usable(5, 6, false));
            assert!(!list_pages_usable(0, 1, false));
            assert!(!list_pages_usable(150, 10, true));
            // no page at all (nothing listed) is fine
            assert!(list_pages_usable(0, 0, false));
        }

        #[test]
        fn test_parse_release_date() {
            assert_eq!(
                parse_release_date("2026年09月19日(土)", Language::Japanese).as_deref(),
                Some("2026-09-19")
            );
            assert_eq!(
                parse_release_date(" July 11, 2025 ", Language::English).as_deref(),
                Some("2025-07-11")
            );
            assert_eq!(
                parse_release_date("Sept. 5, 2025", Language::English).as_deref(),
                Some("2025-09-05")
            );
            assert_eq!(parse_release_date("TBA", Language::English), None);
            assert_eq!(parse_release_date("近日発売", Language::Japanese), None);
        }
    }
}

pub fn update_oshi_skills(
    card: &mut Card,
    mut oshi_skills: Vec<OshiSkill>,
    language: Language,
    update_structure: bool,
    update_text: bool,
) {
    let orig_oshi_skills = &mut card.oshi_skills;
    let new_oshi_skills = &mut oshi_skills;
    let mut copy_language = language;

    if update_structure {
        // Use the new oshi skills as base and copy the other language from the original
        copy_language = match language {
            Language::Japanese => Language::English,
            Language::English => Language::Japanese,
        };
        // Fully replace the original oshi skills with the new ones
        std::mem::swap(orig_oshi_skills, new_oshi_skills);
    } else {
        // If the number of sheet oshi skills is different from the original, warn
        if new_oshi_skills.len() != orig_oshi_skills.len() {
            eprintln!(
                "Warning: {} has a different number of oshi skills than in the original card: {} should be {}",
                card.card_number,
                new_oshi_skills.len(),
                orig_oshi_skills.len()
            );
        }
    }

    for (orig_skill, new_skill) in orig_oshi_skills.iter_mut().zip(new_oshi_skills.iter_mut()) {
        if orig_skill.kind != new_skill.kind {
            eprintln!(
                "Warning: {} oshi skill kind mismatch: {:?} should be {:?}",
                card.card_number, new_skill.kind, orig_skill.kind
            );
        }

        if orig_skill.holo_power != new_skill.holo_power {
            eprintln!(
                "Warning: {} oshi skill holo power mismatch: {} should be {}",
                card.card_number,
                new_skill
                    .holo_power
                    .map(String::from)
                    .unwrap_or("<none>".into()),
                orig_skill
                    .holo_power
                    .map(String::from)
                    .unwrap_or("<none>".into())
            );
        }

        // Update the text in the specified language
        if update_text {
            *orig_skill.name.value_mut(copy_language) =
                new_skill.name.value_mut(copy_language).take();
            *orig_skill.ability_text.value_mut(copy_language) =
                new_skill.ability_text.value_mut(copy_language).take();
        }
    }
}

pub fn update_keywords(
    card: &mut Card,
    mut keywords: Vec<Keyword>,
    language: Language,
    update_structure: bool,
    update_text: bool,
) {
    let orig_keywords = &mut card.keywords;
    let new_keywords = &mut keywords;
    let mut copy_language = language;

    if update_structure {
        // Use the new keywords as base and copy the other language from the original
        copy_language = match language {
            Language::Japanese => Language::English,
            Language::English => Language::Japanese,
        };
        // Fully replace the original keywords with the new ones
        std::mem::swap(orig_keywords, new_keywords);
    } else {
        // If the number of sheet keywords is different from the original, warn
        if new_keywords.len() != orig_keywords.len() {
            eprintln!(
                "Warning: {} has a different number of keywords than in the original card: {} should be {}",
                card.card_number,
                new_keywords.len(),
                orig_keywords.len()
            );
        }
    }

    for (orig_keyword, new_keyword) in orig_keywords.iter_mut().zip(new_keywords.iter_mut()) {
        if orig_keyword.effect != new_keyword.effect {
            eprintln!(
                "Warning: {} keyword effect mismatch: {:?} should be {:?}",
                card.card_number, new_keyword.effect, orig_keyword.effect
            );
        }

        // Update the text in the specified language
        if update_text {
            *orig_keyword.name.value_mut(copy_language) =
                new_keyword.name.value_mut(copy_language).take();
            *orig_keyword.ability_text.value_mut(copy_language) =
                new_keyword.ability_text.value_mut(copy_language).take();
        }
    }
}

pub fn update_arts(
    card: &mut Card,
    mut arts: Vec<Art>,
    language: Language,
    update_structure: bool,
    update_text: bool,
) {
    let orig_arts = &mut card.arts;
    let new_arts = &mut arts;
    let mut copy_language = language;

    if update_structure {
        // Use the new arts as base and copy the other language from the original
        copy_language = match language {
            Language::Japanese => Language::English,
            Language::English => Language::Japanese,
        };
        // Fully replace the original arts with the new ones
        std::mem::swap(orig_arts, new_arts);
    } else {
        // If the number of sheet arts is different from the original, warn
        if new_arts.len() != orig_arts.len() {
            eprintln!(
                "Warning: {} has a different number of arts than in the original card: {} should be {}",
                card.card_number,
                new_arts.len(),
                orig_arts.len()
            );
        }
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
        if update_text {
            *orig_art.name.value_mut(copy_language) = new_art.name.value_mut(copy_language).take();
        }

        if let Some(orig_ability_text) = &mut orig_art.ability_text {
            if let Some(new_ability_text) = &mut new_art.ability_text {
                // Update the text in the specified language
                if update_text {
                    *orig_ability_text.value_mut(copy_language) =
                        new_ability_text.value_mut(copy_language).take();
                }
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

pub fn update_tags(
    card: &mut Card,
    mut tags: Vec<Tag>,
    language: Language,
    update_structure: bool,
    update_text: bool,
) {
    let orig_tags = &mut card.tags;
    let new_tags = &mut tags;
    let mut copy_language = language;

    if update_structure {
        // Use the new tags as base and copy the other language from the original
        copy_language = match language {
            Language::Japanese => Language::English,
            Language::English => Language::Japanese,
        };
        // Fully replace the original tags with the new ones
        std::mem::swap(orig_tags, new_tags);
    } else {
        // If the number of sheet tags is different from the original, warn
        if new_tags.len() != orig_tags.len() {
            eprintln!(
                "Warning: {} has a different number of tags than in the original card: {} should be {}",
                card.card_number,
                new_tags.len(),
                orig_tags.len()
            );
        }
    }

    for (orig_tag, new_tag) in orig_tags.iter_mut().zip(new_tags.iter_mut()) {
        // Update the text in the specified language
        if update_text {
            *orig_tag.value_mut(copy_language) = new_tag.value_mut(copy_language).take();
        }
    }
}

pub fn update_extra(
    card: &mut Card,
    mut extra: Option<Extra>,
    language: Language,
    update_structure: bool,
    update_text: bool,
) {
    let orig_extra = &mut card.extra;
    let new_extra = &mut extra;
    let mut copy_language = language;

    if update_structure {
        // Use the new extra as base and copy the other language from the original
        copy_language = match language {
            Language::Japanese => Language::English,
            Language::English => Language::Japanese,
        };
        // Fully replace the original extra with the new one
        std::mem::swap(orig_extra, new_extra);
    } else {
        // If the number of sheet extra is different from the original, warn
        if new_extra.is_some() != orig_extra.is_some() {
            eprintln!(
                "Warning: {} has a different extra than in the original card: {} should be {}",
                card.card_number,
                new_extra.is_some(),
                orig_extra.is_some()
            );
        }
    }

    for (orig_extra, new_extra) in orig_extra.iter_mut().zip(new_extra.iter_mut()) {
        // Update the text in the specified language
        if update_text {
            *orig_extra.value_mut(copy_language) = new_extra.value_mut(copy_language).take();
        }
    }
}

/// Gives the cheers without a name or a text the ones of the other cheers: the official site
/// only has them for the cheers released in a language, and the sheet doesn't have all the
/// cheers. All the cheers have the same text (a reminder of the rules of the cheers), and the
/// cheers of a set (hY01 white, hY02 green, ...) the same name. A value is the one of the cheers
/// that have it, when they all agree (otherwise a warning, and nothing is filled). Returns the
/// number of filled values.
pub fn fill_cheer_basics(all_cards: &mut hocg_fan_sim_assets_model::CardsDatabase) -> usize {
    fill_cheer_values(
        all_cards,
        "text",
        |_| String::new(),
        |card| &mut card.ability_text,
    ) + fill_cheer_values(
        all_cards,
        "name",
        |number| {
            number
                .split_once('-')
                .map(|(set, _)| set)
                .unwrap_or(number)
                .to_owned()
        },
        |card| &mut card.name,
    )
}

/// Fills a `field` of the cheers without it, with the value the cheers of the same `group` (by
/// card number) have, see [`fill_cheer_basics`].
fn fill_cheer_values(
    all_cards: &mut hocg_fan_sim_assets_model::CardsDatabase,
    what: &str,
    group: impl Fn(&str) -> String,
    field: impl Fn(&mut Card) -> &mut hocg_fan_sim_assets_model::Localized<String>,
) -> usize {
    use hocg_fan_sim_assets_model::CardType;
    use itertools::Itertools;
    use std::collections::BTreeMap;

    let is_empty = |value: &Option<String>| value.as_deref().is_none_or(|v| v.trim().is_empty());
    let mut filled_count = 0;
    for language in [Language::Japanese, Language::English] {
        let mut values: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for card in all_cards
            .values_mut()
            .filter(|card| card.card_type == CardType::Cheer)
        {
            let group = group(&card.card_number);
            let value = field(card).value(language).clone();
            let group_values = values.entry(group).or_default();
            if let Some(value) = value.filter(|v| !v.trim().is_empty())
                && !group_values.contains(&value)
            {
                group_values.push(value);
            }
        }
        for card in all_cards
            .values_mut()
            .filter(|card| card.card_type == CardType::Cheer)
        {
            let group = group(&card.card_number);
            let value = field(card).value_mut(language);
            if !is_empty(value) {
                continue;
            }
            match &values[&group][..] {
                [shared] => {
                    *value = Some(shared.clone());
                    filled_count += 1;
                }
                [] => {}
                different => eprintln!(
                    "Warning: cheer {} without a {language:?} {what}, the cheers{} have different ones: {}",
                    card.card_number,
                    if group.is_empty() {
                        String::new()
                    } else {
                        format!(" of {group}")
                    },
                    different.iter().map(|v| format!("{v:?}")).join(", ")
                ),
            }
        }
    }
    filled_count
}

#[cfg(test)]
mod cheer_tests {
    use hocg_fan_sim_assets_model::{
        Card, CardType, CardsDatabase, Language, Localized, SupportType,
    };

    use super::fill_cheer_basics;

    fn cheer(number: &str, name: Localized<String>, text: Localized<String>) -> (String, Card) {
        (
            number.into(),
            Card {
                card_number: number.into(),
                card_type: CardType::Cheer,
                name,
                ability_text: text,
                ..Default::default()
            },
        )
    }

    #[test]
    fn test_fill_cheer_basics() {
        let jp = "■ホロメンがステージから離れる時...".to_string();
        let en = "■If a holomem leaves the stage...".to_string();
        let both = || Localized::both(jp.clone(), en.clone());
        let white = || Localized::both("白エール".to_string(), "White Cheer".to_string());
        let green = || Localized::both("緑エール".to_string(), "Green Cheer".to_string());
        let mut all_cards: CardsDatabase = [
            cheer("hY01-001", white(), both()),
            cheer(
                "hY01-002",
                Localized::jp("白エール".into()),
                Localized::jp(jp.clone()),
            ),
            cheer(
                "hY01-008",
                Localized::en("White Cheer".into()),
                Localized::en(en.clone()),
            ),
            cheer("hY01-015", Localized::default(), Localized::default()),
            cheer("hY02-001", green(), Localized::jp(" ".into())),
            cheer("hY02-013", Localized::default(), Localized::default()),
            (
                "hSD01-016".into(),
                Card {
                    card_number: "hSD01-016".into(),
                    card_type: CardType::Support(SupportType::Staff),
                    ..Default::default()
                },
            ),
        ]
        .into_iter()
        .collect();

        // texts: 1 + 1 + 2 + 2 + 2, names: 1 + 1 + 2 + 2
        assert_eq!(fill_cheer_basics(&mut all_cards), 14);
        for number in ["hY01-001", "hY01-002", "hY01-008", "hY01-015"] {
            assert_eq!(all_cards[number].ability_text, both(), "{number}");
            assert_eq!(all_cards[number].name, white(), "{number}");
        }
        for number in ["hY02-001", "hY02-013"] {
            assert_eq!(all_cards[number].ability_text, both(), "{number}");
            assert_eq!(all_cards[number].name, green(), "{number}");
        }
        // not a cheer
        assert_eq!(all_cards["hSD01-016"].ability_text, Localized::default());
        assert_eq!(all_cards["hSD01-016"].name, Localized::default());

        // the cheers disagree: nothing is filled
        all_cards.insert(
            "hY01-016".into(),
            cheer("hY01-016", Localized::default(), Localized::default()).1,
        );
        all_cards.get_mut("hY01-001").unwrap().ability_text.japanese = Some("another".into());
        all_cards.get_mut("hY01-001").unwrap().name.english = Some("Whiter Cheer".into());
        assert_eq!(fill_cheer_basics(&mut all_cards), 2);
        let card = &all_cards["hY01-016"];
        assert_eq!(card.ability_text.japanese, None);
        assert_eq!(card.ability_text.value(Language::English), &Some(en));
        assert_eq!(card.name, Localized::jp("白エール".into()));
    }
}
