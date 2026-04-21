use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Display;
use std::ops::Deref;
use std::{cmp::Ordering, num::ParseIntError};

use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};

pub type CardsDatabase = BTreeMap<String, Card>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Japanese,
    English,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Localized<T: Eq + Serialize> {
    #[serde(skip_serializing_if = "is_default")]
    #[serde(rename = "jp")]
    pub japanese: Option<T>,
    #[serde(skip_serializing_if = "is_default")]
    #[serde(rename = "en")]
    pub english: Option<T>,
}

impl<T: Eq + Serialize> Localized<T> {
    pub fn new(language: Language, value: T) -> Self {
        match language {
            Language::Japanese => Localized::jp(value),
            Language::English => Localized::en(value),
        }
    }

    pub fn both(jp: T, en: T) -> Self {
        Self {
            japanese: Some(jp),
            english: Some(en),
        }
    }

    pub fn jp(jp: T) -> Self {
        Self {
            japanese: Some(jp),
            english: None,
        }
    }

    pub fn en(en: T) -> Self {
        Self {
            japanese: None,
            english: Some(en),
        }
    }

    pub fn value(&self, language: Language) -> &Option<T> {
        match language {
            Language::Japanese => &self.japanese,
            Language::English => &self.english,
        }
    }

    pub fn value_mut(&mut self, language: Language) -> &mut Option<T> {
        match language {
            Language::Japanese => &mut self.japanese,
            Language::English => &mut self.english,
        }
    }

    pub fn has_value(&self) -> bool {
        self.japanese.is_some() || self.english.is_some()
    }

    /// Serializes the `Localized` struct to a JSON object with both languages.
    fn full_serialize<S>(x: &Self, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut rgb = s.serialize_struct("Localized", 3)?;
        rgb.serialize_field("jp", &x.japanese)?;
        rgb.serialize_field("en", &x.english)?;
        rgb.end()
    }
}

impl<T: Eq + Serialize + Ord> PartialOrd for Localized<T> {
    fn partial_cmp(&self, other: &Localized<T>) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<T: Eq + Serialize + Ord> Ord for Localized<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        // use Reverse to pull None at the end, while keeping original order for Some values
        (
            Reverse(self.japanese.as_ref().map(Reverse)),
            Reverse(self.english.as_ref().map(Reverse)),
        )
            .cmp(&(
                Reverse(other.japanese.as_ref().map(Reverse)),
                Reverse(other.english.as_ref().map(Reverse)),
            ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CardType {
    #[serde(rename = "oshi_holomem")]
    OshiHoloMember,
    #[serde(rename = "holomem")]
    HoloMember,
    Support(SupportType),
    Cheer,
    #[default]
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportType {
    Staff,
    Item,
    Event,
    Tool,
    Mascot,
    Fan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Color {
    White,
    Green,
    Red,
    Blue,
    Purple,
    Yellow,
    Colorless,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BloomLevel {
    Debut,
    First,
    Second,
    Spot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
#[serde(default)]
pub struct Card {
    pub card_number: String,
    #[serde(serialize_with = "Localized::full_serialize")]
    pub name: Localized<String>,
    pub card_type: CardType,
    #[serde(skip_serializing_if = "is_default")]
    pub colors: Vec<Color>, // oshi, holomem
    #[serde(skip_serializing_if = "is_default")]
    pub life: u32, // oshi
    #[serde(skip_serializing_if = "is_default")]
    pub hp: u32, // holomem
    #[serde(skip_serializing_if = "is_default")]
    pub bloom_level: Option<BloomLevel>, // holomem
    #[serde(skip_serializing_if = "is_default")]
    pub buzz: bool, // holomem
    #[serde(skip_serializing_if = "is_default")]
    pub limited: bool, // support
    #[serde(skip_serializing_if = "is_default")]
    pub oshi_skills: Vec<OshiSkill>, // oshi
    #[serde(skip_serializing_if = "is_default")]
    pub keywords: Vec<Keyword>, // holomem
    #[serde(skip_serializing_if = "is_default")]
    pub arts: Vec<Art>, // holomem
    #[serde(skip_serializing_if = "is_default")]
    #[serde(rename = "text")]
    pub ability_text: AbilityText, // support, cheer
    #[serde(skip_serializing_if = "is_default")]
    pub extra: Option<Extra>, // holomem, support
    #[serde(skip_serializing_if = "is_default")]
    pub tags: Vec<Tag>, // holomem, support
    #[serde(skip_serializing_if = "is_default")]
    pub baton_pass: Vec<Color>, // holomem
    pub max_amount: Localized<u32>, //different restrictions per language
    pub illustrations: Vec<CardIllustration>,
}

impl Card {
    pub fn names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = [self.name.japanese.as_deref(), self.name.english.as_deref()]
            .into_iter()
            .flatten()
            .collect();
        names.dedup();

        // handle FUWAMOCO oshi
        if self.card_type == CardType::OshiHoloMember
            && names.iter().any(|&name| name.contains("FUWAMOCO"))
        {
            for name in [
                "フワワ・アビスガード",
                "モココ・アビスガード",
                "Fuwawa Abyssgard",
                "Mococo Abyssgard",
            ] {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }

        // handle "このホロメンは〈ときのそら〉〈AZKi〉としても扱う"
        // handle "This holomem is also regarded as 〈Tokino Sora〉〈AZKi〉"
        if let Some(extra) = &self.extra
            && (extra
                .japanese
                .as_ref()
                .is_some_and(|jp| jp.contains("このホロメンは") && jp.contains("としても扱う"))
                || extra
                    .english
                    .as_ref()
                    .map(|en| en.to_lowercase())
                    .is_some_and(|en| en.contains("this holomem is also regarded as")))
        {
            // extract names between 〈〉 and <>
            for extra in [extra.japanese.as_deref(), extra.english.as_deref()]
                .into_iter()
                .flatten()
            {
                for part in extra.split('〈').skip(1) {
                    if let Some(name) = part.split('〉').next()
                        && !names.contains(&name)
                    {
                        names.push(name);
                    }
                }
                for part in extra.split('<').skip(1) {
                    if let Some(name) = part.split('>').next()
                        && !names.contains(&name)
                    {
                        names.push(name);
                    }
                }
            }
        }

        names
    }
}

// Oshi skills
// - kind
// - holo power
// - name
// - ability text
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
#[serde(default)]
pub struct OshiSkill {
    #[serde(skip_serializing_if = "is_default")]
    pub kind: OshiSkillKind,
    #[serde(skip_serializing_if = "is_default")]
    pub holo_power: Option<HoloPower>, // normal and special skills only
    #[serde(serialize_with = "Localized::full_serialize")]
    pub name: Localized<String>,
    #[serde(rename = "text")]
    pub ability_text: AbilityText,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OshiSkillKind {
    #[default]
    Normal,
    Special,
    Stage,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HoloPower {
    X,
    #[serde(untagged)]
    Basic(u32),
}

impl TryFrom<String> for HoloPower {
    type Error = ParseIntError;

    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        if value.eq_ignore_ascii_case("x") {
            Ok(Self::X)
        } else {
            Ok(Self::Basic(value.parse()?))
        }
    }
}

impl From<HoloPower> for String {
    fn from(value: HoloPower) -> Self {
        match value {
            HoloPower::Basic(dmg) => format!("{dmg}"),
            HoloPower::X => "x".into(),
        }
    }
}

// Keywords
// - collab effect
// - bloom effect
// - gift
// - name
// - ability text
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
#[serde(default)]
pub struct Keyword {
    #[serde(rename = "type")]
    pub effect: KeywordEffect,
    #[serde(serialize_with = "Localized::full_serialize")]
    pub name: Localized<String>,
    #[serde(rename = "text")]
    pub ability_text: AbilityText,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum KeywordEffect {
    Collab,
    Bloom,
    Gift,
    #[default]
    Other,
}

// Arts
// - cheers
// - name
// - basic power
// - advantage
// - ability text
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
#[serde(default)]
pub struct Art {
    pub cheers: Vec<Color>,
    #[serde(serialize_with = "Localized::full_serialize")]
    pub name: Localized<String>,
    #[serde(skip_serializing_if = "is_default")]
    pub power: ArtPower,
    #[serde(skip_serializing_if = "is_default")]
    pub advantage: Option<(Color, u32)>,
    #[serde(skip_serializing_if = "is_default")]
    #[serde(rename = "text")]
    pub ability_text: Option<AbilityText>,
}

#[derive(Serialize, Deserialize, Debug, Copy, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
#[serde(into = "String")]
#[serde(try_from = "String")]
pub enum ArtPower {
    Basic(u32),
    Plus(u32),
    Minus(u32),
    Multiple(u32),
    #[default]
    Uncertain,
}

impl TryFrom<String> for ArtPower {
    type Error = ParseIntError;

    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        if value == "?" {
            Ok(Self::Uncertain)
        } else if value.ends_with('+') {
            Ok(Self::Plus(value.trim_end_matches('+').parse()?))
        } else if value.ends_with('-') {
            Ok(Self::Minus(value.trim_end_matches('-').parse()?))
        } else if value.ends_with('x') {
            Ok(Self::Multiple(value.trim_end_matches('x').parse()?))
        } else {
            Ok(Self::Basic(value.parse()?))
        }
    }
}

impl From<ArtPower> for String {
    fn from(value: ArtPower) -> Self {
        match value {
            ArtPower::Basic(dmg) => format!("{dmg}"),
            ArtPower::Plus(dmg) => format!("{dmg}+"),
            ArtPower::Minus(dmg) => format!("{dmg}-"),
            ArtPower::Multiple(dmg) => format!("{dmg}x"),
            ArtPower::Uncertain => "?".into(),
        }
    }
}

// Ability text
pub type AbilityText = Localized<String>;

// Extras
pub type Extra = Localized<String>;

// Tags
pub type Tag = Localized<String>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
#[serde(default)]
pub struct CardIllustration {
    pub card_number: String,
    pub manage_id: Localized<BTreeSet<u32>>, // unique ids in Deck Log
    pub rarity: String,
    pub illustrator: Option<String>,
    #[serde(serialize_with = "Localized::full_serialize")]
    pub img_path: Localized<String>,
    #[serde(skip_serializing_if = "is_default")]
    pub img_last_modified: Localized<String>,
    #[serde(skip_serializing_if = "is_default")]
    pub img_hash: String,
    pub ogbajoj_sheet_cells: Option<BTreeSet<(SheetId, SheetCell)>>,
    pub yuyutei_sell_url: Option<String>,
    pub tcgplayer_product_id: Option<u32>,
    pub delta_art_index: Option<u32>,
}

impl CardIllustration {
    pub fn official_site_urls(&self, language: Language) -> Vec<String> {
        self.manage_id
            .value(language)
            .iter()
            .flatten()
            .map(|id| match language {
                Language::Japanese => {
                    format!("https://hololive-official-cardgame.com/cardlist/?id={id}")
                }
                Language::English => {
                    format!("https://en.hololive-official-cardgame.com/cardlist/?id={id}")
                }
            })
            .collect()
    }

    pub fn tcgplayer_url(&self) -> Option<String> {
        self.tcgplayer_product_id
            .map(|id| format!("https://www.tcgplayer.com/product/{id}"))
    }

    pub fn ogbajoj_sheet_urls(&self) -> Vec<String> {
        const SPREADSHEET_ID: &str = "1IdaueY-Jw8JXjYLOhA9hUd2w0VRBao9Z1URJwmCWJ64";
        self.ogbajoj_sheet_cells
            .iter()
            .flatten()
            .map(|(sheet_id, cell)| {
                format!(
                    "https://docs.google.com/spreadsheets/d/{}/view?gid={}#range={}",
                    SPREADSHEET_ID, sheet_id, cell
                )
            })
            .collect()
    }
}

pub type SheetId = u64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SheetCell(String);

impl SheetCell {
    /// Creates a `SheetCell` from 1-based row and column indices.
    pub fn from_row_col(row: usize, col: usize) -> Self {
        let mut col = col - 1; // convert to 0-based index
        let mut col_str = String::new();

        loop {
            let remainder = col % 26;
            col_str.insert(0, (b'A' + remainder as u8) as char);
            if col < 26 {
                break;
            }
            col = col / 26 - 1;
        }

        SheetCell(format!("{}{}", col_str, row))
    }

    /// Converts the `SheetCell` to 1-based row and column indices.
    pub fn to_row_col(&self) -> Option<(usize, usize)> {
        let mut col = 0;
        let mut row_str = String::new();

        for c in self.0.chars() {
            if c.is_ascii_alphabetic() {
                col = col * 26 + (c.to_ascii_uppercase() as usize - 'A' as usize + 1);
            } else if c.is_ascii_digit() {
                row_str.push(c);
            } else {
                return None; // invalid character
            }
        }

        let row: usize = row_str.parse().ok()?;
        Some((row, col)) // 1-based index
    }
}

impl Deref for SheetCell {
    type Target = String;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl PartialOrd for SheetCell {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SheetCell {
    fn cmp(&self, other: &Self) -> Ordering {
        // use Reverse to pull None at the end, while keeping original order for Some values
        (Reverse(self.to_row_col().as_ref().map(Reverse)),)
            .cmp(&(Reverse(other.to_row_col().as_ref().map(Reverse)),))
    }
}

impl From<String> for SheetCell {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl Display for SheetCell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

fn is_default<T: Default + Eq>(value: &T) -> bool {
    value == &T::default()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardOrderingHelper<'c, 'o> {
    card: &'c Card,
    options: &'o CardOrderingOptions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardOrderingOptions {
    ordering_fields: Vec<CardOrderingField>,
    oshi_bias: Option<Card>,
    color_bias: Vec<Color>,
    tag_bias: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardOrderingField {
    CardType,
    Colors,
    Member,
    BloomLevel,
    Buzz,
    AttachText,
    CardNumber,
}

impl Default for CardOrderingOptions {
    fn default() -> Self {
        use CardOrderingField::*;
        CardOrderingOptions {
            ordering_fields: vec![
                CardType, Colors, Member, BloomLevel, Buzz, AttachText, CardNumber,
            ],
            oshi_bias: None,
            color_bias: vec![],
            tag_bias: vec![],
        }
    }
}

impl CardOrderingOptions {
    pub fn member_first() -> CardOrderingOptions {
        use CardOrderingField::*;
        CardOrderingOptions {
            ordering_fields: vec![
                CardType, Member, BloomLevel, Buzz, Colors, AttachText, CardNumber,
            ],
            ..Self::default()
        }
    }

    pub fn deck_sort(
        oshi_bias: Option<Card>,
        color_bias: Vec<Color>,
        tag_bias: Vec<String>,
    ) -> CardOrderingOptions {
        CardOrderingOptions {
            oshi_bias,
            color_bias,
            tag_bias,
            ..Self::member_first()
        }
    }

    pub fn for_card<'o, 'c>(&'o self, card: &'c Card) -> CardOrderingHelper<'c, 'o> {
        CardOrderingHelper {
            card,
            options: self,
        }
    }

    fn holomem_order(&self, text: &str) -> usize {
        // following the order of the official website
        // https://hololive.hololivepro.com/en/talents
        let names = [
            // Gen 0
            "ときのそら",
            "Tokino Sora",
            "ロボ子さん",
            "Robocosan",
            "AZKi",
            "AZKi",
            "さくらみこ",
            "Sakura Miko",
            "星街すいせい",
            "Hoshimachi Suisei",
            // Gen 1
            "アキ・ローゼンタール",
            "Aki Rosenthal",
            "赤井はあと",
            "Akai Haato",
            "白上フブキ",
            "Shirakami Fubuki",
            "夏色まつり",
            "Natsuiro Matsuri",
            // Gen 2
            "紫咲シオン",
            "Murasaki Shion",
            "百鬼あやめ",
            "Nakiri Ayame",
            "癒月ちょこ",
            "Yuzuki Choco",
            "大空スバル",
            "Oozora Subaru",
            "湊あくあ",
            "Minato Aqua",
            // GAMERS
            "大神ミオ",
            "Ookami Mio",
            "猫又おかゆ",
            "Nekomata Okayu",
            "戌神ころね",
            "Inugami Korone",
            // Gen 3
            "兎田ぺこら",
            "Usada Pekora",
            "不知火フレア",
            "Shiranui Flare",
            "白銀ノエル",
            "Shirogane Noel",
            "宝鐘マリン",
            "Houshou Marine",
            // Gen 4
            "天音かなた",
            "Amane Kanata",
            "角巻わため",
            "Tsunomaki Watame",
            "常闇トワ",
            "Tokoyami Towa",
            "姫森ルーナ",
            "Himemori Luna",
            "桐生ココ",
            "Kiryu Coco",
            // Gen 5
            "雪花ラミィ",
            "Yukihana Lamy",
            "桃鈴ねね",
            "Momosuzu Nene",
            "獅白ぼたん",
            "Shishiro Botan",
            "尾丸ポルカ",
            "Omaru Polka",
            // holoX
            "ラプラス・ダークネス",
            "La+ Darknesss",
            "鷹嶺ルイ",
            "Takane Lui",
            "博衣こより",
            "Hakui Koyori",
            "沙花叉クロヱ",
            "Sakamata Chloe",
            "風真いろは",
            "Kazama Iroha",
            // Indonesia
            "アユンダ・リス",
            "Ayunda Risu",
            "ムーナ・ホシノヴァ",
            "Moona Hoshinova",
            "アイラニ・イオフィフティーン",
            "Airani Iofifteen",
            "クレイジー・オリー",
            "Kureiji Ollie",
            "アーニャ・メルフィッサ",
            "Anya Melfissa",
            "パヴォリア・レイネ",
            "Pavolia Reine",
            "ベスティア・ゼータ",
            "Vestia Zeta",
            "カエラ・コヴァルスキア",
            "Kaela Kovalskia",
            "こぼ・かなえる",
            "Kobo Kanaeru",
            // English - Myth
            "森カリオペ",
            "Mori Calliope",
            "小鳥遊キアラ",
            "Takanashi Kiara",
            "一伊那尓栖",
            "Ninomae Ina'nis",
            "がうる・ぐら",
            "Gawr Gura",
            "ワトソン・アメリア",
            "Watson Amelia",
            // Project: HOPE
            "IRyS",
            "IRyS",
            // Council
            "オーロ・クロニー",
            "Ouro Kronii",
            "七詩ムメイ",
            "Nanashi Mumei",
            "ハコス・ベールズ",
            "Hakos Baelz",
            "九十九佐命",
            "Tsukumo Sana",
            "セレス・ファウナ",
            "Ceres Fauna",
            // Advent
            "シオリ・ノヴェラ",
            "Shiori Novella",
            "古石ビジュー",
            "Koseki Bijou",
            "ネリッサ・レイヴンクロフト",
            "Nerissa Ravencroft",
            "フワワ・アビスガード",
            "Fuwawa Abyssgard",
            "モココ・アビスガード",
            "Mococo Abyssgard",
            // Justice
            "エリザベス・ローズ・ブラッドフレイム",
            "Elizabeth Rose Bloodflame",
            "ジジ・ムリン",
            "Gigi Murin",
            "セシリア・イマーグリーン",
            "Cecilia Immergreen",
            "ラオーラ・パンテーラ",
            "Raora Panthera",
            // DEV_IS - ReGLOSS
            "火威青",
            "Hiodoshi Ao",
            "音乃瀬奏",
            "Otonose Kanade",
            "一条莉々華",
            "Ichijou Ririka",
            "儒烏風亭らでん",
            "Juufuutei Raden",
            "轟はじめ",
            "Todoroki Hajime",
            // FLOW GLOW
            "響咲リオナ",
            "Isaki Riona",
            "虎金妃笑虎",
            "Koganei Niko",
            "水宮枢",
            "Mizumiya Su",
            "輪堂千速",
            "Rindo Chihaya",
            "綺々羅々ヴィヴィ",
            "Kikirara Vivi",
            // Staff
            "春先のどか",
            "Harusaki Nodoka",
            "友人A（えーちゃん）",
            "Friend A (A-chan)",
        ];

        // Check if the text contains any of the names, with a bias towards the oshi's names if applicable
        let text = text.to_lowercase();
        self.oshi_bias
            .as_ref()
            .into_iter()
            .flat_map(|card| card.names())
            .chain(names)
            .position(|name| text.contains(name.to_lowercase().as_str()))
            .unwrap_or(usize::MAX)
    }

    fn card_type_key(&self, card: &Card) -> (CardType, bool) {
        // put colorless cards at the end, including spots
        (
            card.card_type,
            card.colors.first().unwrap_or(&Color::Colorless) == &Color::Colorless,
        )
    }

    fn color_key<'a>(&self, card: &'a Card) -> (Reverse<Option<bool>>, Reverse<bool>, &'a [Color]) {
        // add oshi bias here by checking if any of the colors match the oshi's colors, and putting those first
        // then put any cheer colors, then the rest
        (
            Reverse(
                self.oshi_bias
                    .as_ref()
                    .map(|o| o.colors.iter().any(|c| card.colors.contains(c))),
            ),
            Reverse(self.color_bias.iter().any(|c| card.colors.contains(c))),
            &card.colors,
        )
    }

    fn member_key(&self, card: &Card) -> (Reverse<usize>, usize) {
        // for members, we want to sort by the order of their names in the official website
        let names = card.names().join(",");
        let member_order = self.holomem_order(&names);

        // then relevant tags like branch, gen, theme, etc.
        let tag_count = card
            .tags
            .iter()
            .filter(|tag| {
                self.tag_bias.iter().any(|bias| {
                    tag.japanese.as_ref() == Some(bias) || tag.english.as_ref() == Some(bias)
                })
            })
            .count();

        (Reverse(tag_count), member_order)
    }
}

impl<'c, 'o> Ord for CardOrderingHelper<'c, 'o> {
    fn cmp(&self, other: &Self) -> Ordering {
        if self.options != other.options {
            panic!("Cannot compare CardOrderingHelper with different options");
        }

        let CardOrderingHelper { card, options } = self;
        let CardOrderingHelper { card: other, .. } = other;

        for field in &options.ordering_fields {
            match field {
                CardOrderingField::CardType => {
                    // Sort by: Card type
                    let self_type = options.card_type_key(card);
                    let other_type = options.card_type_key(other);
                    let type_ordering = self_type.cmp(&other_type);
                    if type_ordering != Ordering::Equal {
                        // println!(
                        //     "Comparing {} ({:?}) with {} ({:?}) -> {:?}",
                        //     card.card_number, card.card_type, other.card_number, other.card_type, type_ordering
                        // );
                        return type_ordering;
                    }
                }
                CardOrderingField::Colors => {
                    // Sort by: Colors
                    let self_colors = options.color_key(card);
                    let other_colors = options.color_key(other);
                    let color_ordering = self_colors.cmp(&other_colors);
                    if color_ordering != Ordering::Equal {
                        // println!(
                        //     "Comparing {} ({:?}) with {} ({:?}) -> {:?}",
                        //     card.card_number, card.colors, other.card_number, other.colors, color_ordering
                        // );
                        return color_ordering;
                    }
                }
                CardOrderingField::Member => {
                    // Sort by: Members
                    if card.card_type == CardType::OshiHoloMember
                        || card.card_type == CardType::HoloMember
                    {
                        let self_member = options.member_key(card);
                        let other_member = options.member_key(other);
                        let member_ordering = self_member.cmp(&other_member);
                        if member_ordering != Ordering::Equal {
                            // println!(
                            //     "Comparing {} ({:?}) with {} ({:?}) -> {:?}",
                            //     self.card_number,
                            //     self.name.japanese,
                            //     other.card_number,
                            //     other.name.japanese,
                            //     member_ordering
                            // );
                            return member_ordering;
                        }
                    }
                }
                CardOrderingField::BloomLevel => {
                    // Sort by: Bloom Level
                    if card.card_type == CardType::HoloMember {
                        let bloom_ordering = card.bloom_level.cmp(&other.bloom_level);
                        if bloom_ordering != Ordering::Equal {
                            // println!(
                            //     "Comparing {} ({:?}) with {} ({:?}) -> {:?}",
                            //     card.card_number,
                            //     card.bloom_level,
                            //     other.card_number,
                            //     other.bloom_level,
                            //     bloom_ordering
                            // );
                            return bloom_ordering;
                        }
                    }
                }
                CardOrderingField::Buzz => {
                    // Sort by: Buzz
                    if card.card_type == CardType::HoloMember {
                        let buzz_ordering = card.buzz.cmp(&other.buzz);
                        if buzz_ordering != Ordering::Equal {
                            // println!(
                            //     "Comparing {} ({:?}) with {} ({:?}) -> {:?}",
                            //     card.card_number, card.buzz, other.card_number, other.buzz, buzz_ordering
                            // );
                            return buzz_ordering;
                        }
                    }
                }
                CardOrderingField::AttachText => {
                    // Sort by: Tool/Mascot/Fan members
                    if card.card_type == CardType::Support(SupportType::Tool)
                        || card.card_type == CardType::Support(SupportType::Mascot)
                        || card.card_type == CardType::Support(SupportType::Fan)
                    {
                        let self_text: String = card
                            .oshi_skills
                            .iter()
                            .flat_map(|skill| {
                                [
                                    skill.ability_text.japanese.as_ref(),
                                    skill.ability_text.english.as_ref(),
                                ]
                            })
                            .chain(card.keywords.iter().flat_map(|keyword| {
                                [
                                    keyword.ability_text.japanese.as_ref(),
                                    keyword.ability_text.english.as_ref(),
                                ]
                            }))
                            .chain(card.arts.iter().flat_map(|art| {
                                art.ability_text
                                    .iter()
                                    .flat_map(|t| [t.japanese.as_ref(), t.english.as_ref()])
                            }))
                            .chain([
                                card.ability_text.japanese.as_ref(),
                                card.ability_text.english.as_ref(),
                            ])
                            .flatten()
                            .cloned()
                            .collect();
                        let other_text: String = other
                            .oshi_skills
                            .iter()
                            .flat_map(|skill| {
                                [
                                    skill.ability_text.japanese.as_ref(),
                                    skill.ability_text.english.as_ref(),
                                ]
                            })
                            .chain(other.keywords.iter().flat_map(|keyword| {
                                [
                                    keyword.ability_text.japanese.as_ref(),
                                    keyword.ability_text.english.as_ref(),
                                ]
                            }))
                            .chain(other.arts.iter().flat_map(|art| {
                                art.ability_text
                                    .iter()
                                    .flat_map(|t| [t.japanese.as_ref(), t.english.as_ref()])
                            }))
                            .chain([
                                other.ability_text.japanese.as_ref(),
                                other.ability_text.english.as_ref(),
                            ])
                            .flatten()
                            .cloned()
                            .collect();

                        let self_order = options.holomem_order(&self_text);
                        let other_order = options.holomem_order(&other_text);
                        let ordering = self_order.cmp(&other_order);
                        if ordering != Ordering::Equal {
                            // println!(
                            //     "Comparing {} ({:?}) with {} ({:?}) -> {:?}",
                            //     card.card_number,
                            //     card.text.japanese,
                            //     other.card_number,
                            //     other.text.japanese,
                            //     ordering
                            // );
                            return ordering;
                        }
                    }
                }
                CardOrderingField::CardNumber => {
                    // Final fallback: compare by card number
                    let ordering = card.card_number.cmp(&other.card_number);
                    if ordering != Ordering::Equal {
                        // println!(
                        //     "Comparing {} with {} -> {:?}",
                        //     card.card_number, other.card_number, ordering
                        // );
                        return ordering;
                    }
                }
            }
        }

        // it's the same card
        Ordering::Equal
    }
}

impl<'c, 'o> PartialOrd for CardOrderingHelper<'c, 'o> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub type QnaDatabase = BTreeMap<QnaNumber, Qna>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
#[serde(default)]
pub struct Qna {
    pub qna_number: String,
    pub date: Option<String>, // YYYY-MM-DD
    #[serde(serialize_with = "Localized::full_serialize")]
    pub question: Localized<String>,
    #[serde(serialize_with = "Localized::full_serialize")]
    pub answer: Localized<String>,
    #[serde(skip_serializing_if = "is_default")]
    pub referenced_cards: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct QnaNumber(String);

impl Deref for QnaNumber {
    type Target = String;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl PartialOrd for QnaNumber {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for QnaNumber {
    fn cmp(&self, other: &Self) -> Ordering {
        let self_num = self.0.trim_start_matches('Q').parse::<u32>().unwrap_or(0);
        let other_num = other.0.trim_start_matches('Q').parse::<u32>().unwrap_or(0);
        self_num.cmp(&other_num)
    }
}

impl From<String> for QnaNumber {
    fn from(value: String) -> Self {
        Self(value)
    }
}
