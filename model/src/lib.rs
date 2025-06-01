use std::collections::BTreeMap;
use std::{cmp::Ordering, num::ParseIntError};

use serde::{Deserialize, Serialize};

pub type CardsDatabase = BTreeMap<String, Card>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Localized<T> {
    #[serde(rename = "jp")]
    pub japanese: Option<T>,
    #[serde(rename = "en")]
    pub english: Option<T>,
}

impl<T> Localized<T> {
    pub fn new(jp: T, en: T) -> Self {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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
    pub extra: Option<Extra>, // holomem
    #[serde(skip_serializing_if = "is_default")]
    pub tags: Vec<Localized<String>>, // holomem, support
    #[serde(skip_serializing_if = "is_default")]
    pub baton_pass: Vec<Color>, // holomem
    pub max_amount: u32,
    pub illustrations: Vec<CardIllustration>,
}

// Oshi skills
// - special
// - holo power
// - name
// - ability text
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
#[serde(default)]
pub struct OshiSkill {
    #[serde(skip_serializing_if = "is_default")]
    pub special: bool,
    pub holo_power: HoloPower,
    pub name: Localized<String>,
    #[serde(rename = "text")]
    pub ability_text: AbilityText,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum HoloPower {
    #[default]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
#[serde(default)]
pub struct CardIllustration {
    pub card_number: String,
    pub manage_id: Option<u32>, // unique id in Deck Log
    pub rarity: String,
    pub illustrator: Option<String>,
    pub img_path: Localized<String>,
    #[serde(skip_serializing_if = "is_default")]
    pub img_last_modified: Option<String>,
    pub yuyutei_sell_url: Option<String>,
    pub delta_art_index: Option<u32>,
}

fn is_default<T: Default + Eq>(value: &T) -> bool {
    value == &T::default()
}

fn holomem_order(text: &str) -> usize {
    // following the order of the official website
    // https://hololive.hololivepro.com/en/talents
    let names = [
        // Gen 0
        "ときのそら",   // Tokino Sora
        "ロボ子さん",   // Robocosan
        "AZKi",         // AZKi
        "さくらみこ",   // Sakura Miko
        "星街すいせい", // Hoshimachi Suisei
        // Gen 1
        "アキ・ローゼンタール", // Aki Rosenthal
        "赤井はあと",           // Akai Haato
        "白上フブキ",           // Shirakami Fubuki
        "夏色まつり",           // Natsuiro Matsuri
        // Gen 2
        "紫咲シオン", // Murasaki Shion
        "百鬼あやめ", // Nakiri Ayame
        "癒月ちょこ", // Yuzuki Choco
        "大空スバル", // Oozora Subaru
        "湊あくあ",   // Minato Aqua
        // GAMERS
        "大神ミオ",   // Ookami Mio
        "猫又おかゆ", // Nekomata Okayu
        "戌神ころね", // Inugami Korone
        // Gen 3
        "兎田ぺこら",   // Usada Pekora
        "不知火フレア", // Shiranui Flare
        "白銀ノエル",   // Shirogane Noel
        "宝鐘マリン",   // Houshou Marine
        // Gen 4
        "天音かなた", // Amane Kanata
        "角巻わため", // Tsunomaki Watame
        "常闇トワ",   // Tokoyami Towa
        "姫森ルーナ", // Himemori Luna
        "桐生ココ",   // Kiryu Coco
        // Gen 5
        "雪花ラミィ", // Yukihana Lamy
        "桃鈴ねね",   // Momosuzu Nene
        "獅白ぼたん", // Shishiro Botan
        "尾丸ポルカ", // Omaru Polka
        // holoX
        "ラプラス・ダークネス", // La+ Darknesss
        "鷹嶺ルイ",             // Takane Lui
        "博衣こより",           // Hakui Koyori
        "沙花叉クロヱ",         // Sakamata Chloe
        "風真いろは",           // Kazama Iroha
        // Indonesia
        "アユンダ・リス",               // Ayunda Risu
        "ムーナ・ホシノヴァ",           // Moona Hoshinova
        "アイラニ・イオフィフティーン", // Airani Iofifteen
        "クレイジー・オリー",           // Kureiji Ollie
        "アーニャ・メルフィッサ",       // Anya Melfissa
        "パヴォリア・レイネ",           // Pavolia Reine
        "ベスティア・ゼータ",           // Vestia Zeta
        "カエラ・コヴァルスキア",       // Kaela Kovalskia
        "こぼ・かなえる",               // Kobo Kanaeru
        // English - Myth
        "森カリオペ",         // Mori Calliope
        "小鳥遊キアラ",       // Takanashi Kiara
        "一伊那尓栖",         // Ninomae Ina'nis
        "がうる・ぐら",       // Gawr Gura
        "ワトソン・アメリア", // Watson Amelia
        // Project: HOPE
        "IRyS", // IRyS
        // Council
        "オーロ・クロニー", // Ouro Kronii
        "七詩ムメイ",       // Nanashi Mumei
        "ハコス・ベールズ", // Hakos Baelz
        "九十九佐命",       // Tsukumo Sana
        "セレス・ファウナ", // Ceres Fauna
        // Advent
        "シオリ・ノヴェラ",           // Shiori Novella
        "古石ビジュー",               // Koseki Bijou
        "ネリッサ・レイヴンクロフト", // Nerissa Ravencroft
        "フワワ・アビスガード",       // Fuwawa Abyssgard
        "モココ・アビスガード",       // Mococo Abyssgard
        // Justice
        "エリザベス・ローズ・ブラッドフレイム", // Elizabeth Rose Bloodflame
        "ジジ・ムリン",                         // Gigi Murin
        "セシリア・イマーグリーン",             // Cecilia Immergreen
        "ラオーラ・パンテーラ",                 // Raora Panthera
        // DEV_IS - ReGLOSS
        "火威青",         // Hiodoshi Ao
        "音乃瀬奏",       // Otonose Kanade
        "一条莉々華",     // Ichijou Ririka
        "儒烏風亭らでん", // Juufuutei Raden
        "轟はじめ",       // Todoroki Hajime
        // FLOW GLOW
        "響咲リオナ",       // Isaki Riona
        "虎金妃笑虎",       // Koganei Niko
        "水宮枢",           // Mizumiya Su
        "輪堂千速",         // Rindo Chihaya
        "綺々羅々ヴィヴィ", // Kikirara Vivi
        // Staff
        "春先のどか",          // Harusaki Nodoka
        "友人A（えーちゃん）", // Friend A (A-chan)
    ];

    // Check if the text contains any of the names
    names
        .iter()
        .position(|&name| text.contains(name))
        .unwrap_or(usize::MAX)
}

impl Ord for Card {
    fn cmp(&self, other: &Self) -> Ordering {
        // Priority 1: Card type
        let type_ordering = self.card_type.cmp(&other.card_type);
        if type_ordering != Ordering::Equal {
            // println!(
            //     "Comparing {} ({:?}) with {} ({:?}) -> {:?}",
            //     self.card_number, self.card_type, other.card_number, other.card_type, type_ordering
            // );
            return type_ordering;
        }

        // Priority 2: Colors
        let color_ordering = self.colors.cmp(&other.colors);
        if color_ordering != Ordering::Equal {
            // println!(
            //     "Comparing {} ({:?}) with {} ({:?}) -> {:?}",
            //     self.card_number, self.colors, other.card_number, other.colors, color_ordering
            // );
            return color_ordering;
        }

        // Priority 3: Members
        if self.card_type == CardType::OshiHoloMember || self.card_type == CardType::HoloMember {
            let self_name: String = self
                .name
                .japanese
                .as_ref()
                .into_iter()
                // unit cards have names in extras
                .chain(self.extra.as_ref().and_then(|e| e.japanese.as_ref()))
                .cloned()
                .collect();
            let other_name: String = other
                .name
                .japanese
                .as_ref()
                .into_iter()
                // unit cards have names in extras
                .chain(other.extra.as_ref().and_then(|e| e.japanese.as_ref()))
                .cloned()
                .collect();

            let self_member_order = holomem_order(&self_name);
            let other_member_order = holomem_order(&other_name);
            let member_ordering = self_member_order.cmp(&other_member_order);
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

            // Priority 4: Bloom Level
            let bloom_ordering = self.bloom_level.cmp(&other.bloom_level);
            if bloom_ordering != Ordering::Equal {
                // println!(
                //     "Comparing {} ({:?}) with {} ({:?}) -> {:?}",
                //     self.card_number,
                //     self.bloom_level,
                //     other.card_number,
                //     other.bloom_level,
                //     bloom_ordering
                // );
                return bloom_ordering;
            }

            // Priority 5: Buzz
            let buzz_ordering = self.buzz.cmp(&other.buzz);
            if buzz_ordering != Ordering::Equal {
                // println!(
                //     "Comparing {} ({:?}) with {} ({:?}) -> {:?}",
                //     self.card_number, self.buzz, other.card_number, other.buzz, buzz_ordering
                // );
                return buzz_ordering;
            }
        }

        // Priority 6: Tool/Mascot/Fan members
        if self.card_type == CardType::Support(SupportType::Tool)
            || self.card_type == CardType::Support(SupportType::Mascot)
            || self.card_type == CardType::Support(SupportType::Fan)
        {
            let self_text: String =
                self.oshi_skills
                    .iter()
                    .flat_map(|skill| &skill.ability_text.japanese)
                    .chain(
                        self.keywords
                            .iter()
                            .flat_map(|keyword| &keyword.ability_text.japanese),
                    )
                    .chain(self.arts.iter().filter_map(|art| {
                        art.ability_text.as_ref().and_then(|t| t.japanese.as_ref())
                    }))
                    .chain(&self.ability_text.japanese)
                    .cloned()
                    .collect();
            let other_text: String =
                other
                    .oshi_skills
                    .iter()
                    .flat_map(|skill| &skill.ability_text.japanese)
                    .chain(
                        other
                            .keywords
                            .iter()
                            .flat_map(|keyword| &keyword.ability_text.japanese),
                    )
                    .chain(other.arts.iter().filter_map(|art| {
                        art.ability_text.as_ref().and_then(|t| t.japanese.as_ref())
                    }))
                    .chain(&other.ability_text.japanese)
                    .cloned()
                    .collect();

            let self_order = holomem_order(&self_text);
            let other_order = holomem_order(&other_text);
            let ordering = self_order.cmp(&other_order);
            if ordering != Ordering::Equal {
                // println!(
                //     "Comparing {} ({:?}) with {} ({:?}) -> {:?}",
                //     self.card_number,
                //     self.text.japanese,
                //     other.card_number,
                //     other.text.japanese,
                //     ordering
                // );
                return ordering;
            }
        }

        // Final fallback: compare by card number
        self.card_number.cmp(&other.card_number)
    }
}

impl PartialOrd for Card {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
