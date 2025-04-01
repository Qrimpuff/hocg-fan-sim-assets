pub mod decklog {

    use std::{fmt::Display, str::FromStr, sync::Arc};

    use hocg_fan_sim_assets_model::{
        BloomLevel, CardIllustration, CardType, CardsInfo2, Localized, SupportType,
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
        pub fn card_type(&self) -> CardType {
            match self.card_kind.trim().to_lowercase().as_str() {
                s if s.contains("推し") => CardType::OshiHolomem,
                s if s.contains("ホロメン") => CardType::Holomem,
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

        pub fn bloom_level(&self) -> Option<BloomLevel> {
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

        pub fn buzz(&self) -> bool {
            self.card_kind.trim().to_lowercase().contains("buzz")
        }

        pub fn limited(&self) -> bool {
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
        all_cards: &mut CardsInfo2,
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
                                    card.name.japanese = dl_card.name.clone();
                                    card.max_amount = dl_card.max;
                                    card.card_type = dl_card.card_type();
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
                                        } else if let Some(i) = illustrations
                                            .iter_mut()
                                            .find(|i| i.img_path.japanese == dl_card.img)
                                        {
                                            Some(i)
                                        } else {
                                            illustrations.iter_mut().find(|c| c.manage_id.is_none())
                                        }
                                    } {
                                        // only these fields are retrieved
                                        illust.card_number = dl_card.card_number;
                                        illust.manage_id = dl_card.manage_id;
                                        illust.rarity = dl_card.rare;
                                        illust.img_path.japanese = dl_card.img;
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

    use hocg_fan_sim_assets_model::{
        BloomLevel, Card, CardType, CardsInfo2, Color, Localized, SupportType,
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
        pub fn update_card(&self, card: &mut Card) {
            // TODO don't overwrite if it already exists
            card.card_number = self.set_code.clone();
            card.name = self.name();
            card.card_type = self.card_type();
            card.colors = self.colors();
            if card.card_type == CardType::OshiHolomem {
                card.life = self.life_hp.parse().unwrap_or_default();
            } else if card.card_type == CardType::Holomem {
                card.hp = self.life_hp.parse().unwrap_or_default();
            }
            card.bloom_level = self.bloom_level();
            card.buzz = self.buzz();
            card.limited = self.limited();
            card.text.english = Some(self.text()).filter(|t| !t.is_empty());
            // there is no japanese text in the sheet
            card.tags = self
                .tags()
                .into_iter()
                .map(|t| Localized::new("".into(), t))
                .collect(); // TODO update existing tags (tags consistency check)
            // there is no baton pass in the sheet
            // there is no max amount in the sheet
        }

        pub fn name(&self) -> Localized<String> {
            if let Some((jp, en)) = self.card_name_jp_en.lines().collect_tuple() {
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

        pub fn card_type(&self) -> CardType {
            match self.card_type.trim().to_lowercase().as_str() {
                s if s.contains("oshi") => CardType::OshiHolomem,
                s if s.contains("holomem") => CardType::Holomem,
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

        pub fn colors(&self) -> Vec<Color> {
            self.color
                .split('/')
                .map(|s| s.trim().to_lowercase())
                .map(|s| match s.as_str() {
                    "white" => Color::White,
                    "green" => Color::Green,
                    "red" => Color::Red,
                    "blue" => Color::Blue,
                    "purple" => Color::Purple,
                    "yellow" => Color::Yellow,
                    _ => Color::ColorLess,
                })
                .collect()
        }

        pub fn bloom_level(&self) -> Option<BloomLevel> {
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

        pub fn buzz(&self) -> bool {
            self.card_type.trim().to_lowercase().contains("buzz")
        }

        pub fn limited(&self) -> bool {
            self.text.trim().to_lowercase().contains("limited:")
        }

        // TODO split text into arts/effects
        pub fn text(&self) -> String {
            // should remove the limited line
            self.text
                .lines()
                .map(|l| l.trim())
                .filter(|l| !l.is_empty())
                .filter(|l| !l.to_lowercase().starts_with("limited:"))
                .collect::<Vec<_>>()
                .join("\n")
                .to_string()
        }

        pub fn tags(&self) -> Vec<String> {
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
    pub fn retrieve_card_info_from_ogbajoj_sheet(all_cards: &mut CardsInfo2) {
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
                    println!("Card {} not found", record.set_code);
                    continue;
                };
                record.update_card(card);
                updated_count += 1;
            }
        }

        println!("Updated {updated_count} cards");
    }
}

pub mod hololive_official {
    // TODO: add support for this
}
