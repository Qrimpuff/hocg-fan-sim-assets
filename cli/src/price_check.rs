use std::{collections::HashMap, time::Duration};

use hocg_fan_sim_assets_model::CardsDatabase;
use indexmap::IndexMap;
use reqwest::Url;
use scraper::{Html, Selector};

use crate::http_client;

pub fn yuyutei(all_cards: &mut CardsDatabase) {
    println!("Scraping Yuyutei urls...");

    let mut urls = IndexMap::new();

    let scraperapi_key = std::env::var("SCRAPERAPI_API_KEY").ok();
    if scraperapi_key.is_some() {
        println!("using scraperapi.com");
    }

    // handle multiple pages (one page is 600 cards)
    // could be slow when there are multiple pages
    let mut page = 1;
    let mut max_page = 1;
    while page <= max_page {
        let mut url = Url::parse("https://yuyu-tei.jp/sell/hocg/s/search").unwrap();
        url.query_pairs_mut()
            .append_pair("search_word", "")
            .append_pair("page", page.to_string().as_str());
        let resp = if let Some(scraperapi_key) = &scraperapi_key {
            http_client()
                .get("https://api.scraperapi.com/")
                .query(&[
                    ("api_key", scraperapi_key.as_str()),
                    ("url", url.as_str()),
                    ("session_number", "123"),
                ])
                .timeout(Duration::from_secs(70))
                .send()
                .unwrap()
        } else {
            http_client().get(url.clone()).send().unwrap()
        };

        let content = resp.text().unwrap();
        // println!("{content}");

        let document = Html::parse_document(&content);
        let card_lists = Selector::parse("#card-list3").unwrap();
        let rarity_select = Selector::parse("h3 span").unwrap();
        let cards_select = Selector::parse(".card-product").unwrap();
        let number_select = Selector::parse("span").unwrap();
        let url_select = Selector::parse("a").unwrap();
        let max_page_select = Selector::parse(".pagination li:nth-last-child(2) a").unwrap();

        for card_list in document.select(&card_lists) {
            let rarity: String = card_list
                .select(&rarity_select)
                .next()
                .unwrap()
                .text()
                .collect();
            for card in card_list.select(&cards_select) {
                let number: String = card.select(&number_select).next().unwrap().text().collect();
                let url = card.select(&url_select).next().unwrap().attr("href");
                if let Some(url) = url {
                    // group them by url
                    urls.entry(url.to_owned())
                        .or_insert((number, rarity.clone()));
                }
            }
        }

        if let Some(max) = document.select(&max_page_select).next() {
            max_page = max.text().collect::<String>().parse().unwrap();
            // println!("price_check: max_page: {max_page}");
        }

        page += 1;
    }
    println!("Found {} Yuyutei urls...", urls.len());

    let mut url_count = 0;
    let mut url_skipped = 0;

    // println!("BEFORE: {urls:#?}");
    // remove existing urls
    let mut existing_urls: HashMap<String, String> = HashMap::new();
    for card in all_cards
        .values_mut()
        .flat_map(|cs| cs.illustrations.iter_mut())
        .filter(|c| c.yuyutei_sell_url.is_some())
    {
        if let Some(yuyutei_sell_url) = &card.yuyutei_sell_url {
            if urls.shift_remove(yuyutei_sell_url).is_some() {
                url_skipped += 1;
            }
            // group by image, some entries are duplicated, like hSD01-016
            existing_urls
                .entry(card.img_path.japanese.clone())
                .or_insert(yuyutei_sell_url.clone());
        }
    }

    // swap keys and values
    let mut urls: HashMap<_, Vec<_>> = urls.into_iter().fold(HashMap::new(), |mut map, (k, v)| {
        map.entry(v).or_default().push(k);
        map
    });

    // warn if there are some card with same number and rarity
    for ((number, rarity), urls) in &urls {
        if urls.len() > 1 {
            println!("WARNING: {number} ({rarity}) has multiple urls: {urls:#?}");
        }
    }

    // println!("BETWEEN: {urls:#?}");
    // add the remaining urls
    for card in all_cards
        .values_mut()
        .flat_map(|cs| cs.illustrations.iter_mut())
        .filter(|c| c.yuyutei_sell_url.is_none())
    {
        // look some same image first
        if let Some(yuyutei_sell_url) = existing_urls.get(&card.img_path.japanese) {
            card.yuyutei_sell_url = Some(yuyutei_sell_url.clone());
        } else if let Some(urls) = urls.get_mut(&(card.card_number.clone(), card.rarity.clone())) {
            if !urls.is_empty() {
                // take the first url (should be in chronological order, with some exceptions)
                let yuyutei_sell_url = urls.remove(0);
                card.yuyutei_sell_url = Some(yuyutei_sell_url.clone());
                // group by image, some entries are duplicated
                existing_urls
                    .entry(card.img_path.japanese.clone())
                    .or_insert(yuyutei_sell_url);
                url_count += 1;
            }
        }
    }

    // remove empty urls
    urls.retain(|_, urls| !urls.is_empty());
    // println!("AFTER: {urls:#?}");

    println!("{url_count} Yuyutei urls updated ({url_skipped} skipped)");
    for ((number, rare), urls) in urls {
        for url in urls {
            println!("MISSING: [{number}, {rare}] - {url}");
        }
    }
}
