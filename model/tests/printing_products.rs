use hocg_fan_sim_assets_model::{
    CardIllustration, CardRarity, CardsDatabase, Language, Localized, Product, ProductsDatabase,
};
use serde::Deserialize;

/// JSON written before `products` / `promo_sources` existed.
const OLD_JSON: &str = r#"{
    "card_number": "hBP09-094",
    "manage_id": { "jp": [2811] },
    "rarity": "U",
    "illustrator": null,
    "img_path": { "jp": "hBP09-094_U.webp", "en": null },
    "ogbajoj_sheet_cells": null,
    "yuyutei_sell_paths": null,
    "tcgplayer_product_ids": null,
    "delta_art_index": null
}"#;

#[test]
fn old_json_loads_without_products() {
    let illust: CardIllustration = serde_json::from_str(OLD_JSON).unwrap();
    assert_eq!(illust.card_number, "hBP09-094");
    assert_eq!(illust.rarity, CardRarity::Uncommon);
    assert_eq!(illust.products, Localized::default());
    assert_eq!(illust.promo_sources, Localized::default());

    // empty values are not written back
    let json = serde_json::to_string(&illust).unwrap();
    assert!(!json.contains("products"), "{json}");
    assert!(!json.contains("promo_sources"), "{json}");
}

#[test]
fn products_are_expansion_codes() {
    let illust = CardIllustration {
        card_number: "hBP01-021".into(),
        rarity: CardRarity::Common,
        products: Localized::both(vec!["hBP01".into()], vec!["hSD01".into(), "hBP01".into()]),
        promo_sources: Localized::en(vec!["Basic PR Pack vol 5".into()]),
        ..Default::default()
    };

    let json = serde_json::to_value(&illust).unwrap();
    assert_eq!(
        json["products"],
        serde_json::json!({ "jp": ["hBP01"], "en": ["hSD01", "hBP01"] })
    );
    assert_eq!(
        json["promo_sources"],
        serde_json::json!({ "en": ["Basic PR Pack vol 5"] })
    );

    let back: CardIllustration = serde_json::from_value(json).unwrap();
    assert_eq!(back, illust);
}

#[test]
fn products_database_round_trips() {
    let products: ProductsDatabase = serde_json::from_str(
        r#"{
            "hBP01": {
                "name": { "jp": "ブースターパック「ブルーミングレディアンス」", "en": "Booster Pack – Blooming Radiance" },
                "release_date": { "jp": "2024-09-20", "en": "2025-07-11" },
                "category": "boosters",
                "url": { "jp": "https://hololive-official-cardgame.com/products/post/blooming-radiance/" }
            },
            "hPR": { "name": { "jp": "PRカード", "en": "PROMO CARDS" }, "category": "pr" }
        }"#,
    )
    .unwrap();
    assert_eq!(
        products["hBP01"]
            .release_date
            .value(Language::English)
            .as_deref(),
        Some("2025-07-11")
    );
    assert_eq!(products["hPR"].release_date, Localized::default());

    // missing values are not written
    let json = serde_json::to_value(&products).unwrap();
    assert_eq!(
        json["hPR"],
        serde_json::json!({ "name": { "jp": "PRカード", "en": "PROMO CARDS" }, "category": "pr" })
    );
    let back: ProductsDatabase = serde_json::from_value(json).unwrap();
    assert_eq!(back, products);
}

#[test]
fn products_in_looks_up_the_catalog() {
    let products = ProductsDatabase::from([(
        "hBP01".to_string(),
        Product {
            name: Localized::en("Booster Pack – Blooming Radiance".into()),
            category: Some("boosters".into()),
            ..Default::default()
        },
    )]);
    let illust = CardIllustration {
        products: Localized::en(vec!["hSD01".into(), "hBP01".into()]),
        ..Default::default()
    };
    assert_eq!(
        illust.products_in(Language::English, &products),
        [("hSD01", None), ("hBP01", Some(&products["hBP01"]))]
    );
    assert!(illust.products_in(Language::Japanese, &products).is_empty());
}

/// Mirror of the struct as old readers know it: unknown fields must be ignored.
#[derive(Deserialize)]
#[allow(dead_code)]
struct OldCardIllustration {
    card_number: String,
    rarity: CardRarity,
    img_path: Localized<String>,
}

#[test]
fn old_readers_ignore_new_fields() {
    let mut illust: CardIllustration = serde_json::from_str(OLD_JSON).unwrap();
    illust.products = Localized::jp(vec!["hPR".into()]);
    illust.promo_sources = Localized::jp(vec!["Basic PR Pack vol 5".into()]);
    let json = serde_json::to_string(&illust).unwrap();
    assert!(json.contains("promo_sources"));

    let old: OldCardIllustration = serde_json::from_str(&json).unwrap();
    assert_eq!(old.card_number, "hBP09-094");
    assert_eq!(old.rarity, CardRarity::Uncommon);

    // the root of hocg_cards.json is unchanged
    let cards: CardsDatabase = serde_json::from_str(&format!(
        r#"{{ "hBP09-094": {{ "card_number": "hBP09-094", "illustrations": [{json}] }} }}"#
    ))
    .unwrap();
    assert_eq!(cards["hBP09-094"].illustrations[0], illust);
}

#[test]
fn display_name_prefers_official_names() {
    let product = Product {
        name: Localized::jp("ブースターパック「ボリュームヴォルテックス」".into()),
        short_name: Localized::jp("ボリュームヴォルテックス".into()),
        unofficial_name: Localized::en("Set 9 Volume Vortex".into()),
        ..Default::default()
    };
    assert_eq!(
        product.display_name(Language::Japanese, true),
        Some(("ボリュームヴォルテックス", true))
    );
    assert_eq!(
        product.display_name(Language::Japanese, false),
        Some(("ブースターパック「ボリュームヴォルテックス」", true))
    );
    assert_eq!(
        product.display_name(Language::English, true),
        Some(("Set 9 Volume Vortex", false))
    );
    // no short name: the full name
    let product = Product {
        name: Localized::en("PROMO CARDS".into()),
        ..Default::default()
    };
    assert_eq!(
        product.display_name(Language::English, true),
        Some(("PROMO CARDS", true))
    );
    assert_eq!(product.display_name(Language::Japanese, true), None);
}

#[test]
fn new_fields_are_skipped_when_empty() {
    let json = serde_json::to_value(Product::default()).unwrap();
    assert_eq!(json, serde_json::json!({}));
    let illust: CardIllustration = serde_json::from_str(OLD_JSON).unwrap();
    assert!(
        !serde_json::to_string(&illust)
            .unwrap()
            .contains("promo_sources")
    );
}
