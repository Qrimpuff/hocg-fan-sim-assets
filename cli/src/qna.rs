use std::sync::{Arc, OnceLock};

use hocg_fan_sim_assets_model::{Language, Qna, QnaDatabase};
use itertools::Itertools;
use parking_lot::{Mutex, RwLock};
use rayon::iter::{ParallelBridge, ParallelIterator};
use reqwest::header::REFERER;
use scraper::{Html, Selector};

use crate::http_client;

pub fn generate_qna(all_qnas: &mut QnaDatabase, language: Language) {
    println!("Retrieve all Q&As from Hololive official site for language: {language:?}");

    let updated_count = Arc::new(Mutex::new(0));
    let all_qnas = Arc::new(RwLock::new(all_qnas));

    // Process pages in parallel
    let _ = (1..)
        .par_bridge()
        .map({
            let updated_count = updated_count.clone();
            let all_qnas = all_qnas.clone();
            move |page| {
                let (url, referrer) = match language {
                    Language::Japanese => (
                        "https://hololive-official-cardgame.com/rules/question/search_ex",
                        "https://hololive-official-cardgame.com/",
                    ),
                    Language::English => (
                        "https://en.hololive-official-cardgame.com/rules/question/search_ex",
                        "https://en.hololive-official-cardgame.com/",
                    ),
                };

                let resp = http_client()
                    .get(url)
                    .query(&[("page", page.to_string().as_str())])
                    .header(REFERER, referrer)
                    .send()
                    .unwrap();

                let content = resp.text().unwrap();

                // no more Q&As in this page
                if content.contains(
                    "<title>hololive OFFICIAL CARD GAME｜ホロライブプロダクション</title>",
                ) || content
                    .contains("<title>hololive OFFICIAL CARD GAME｜hololive production</title>")
                    || content.contains("Page not found")
                {
                    return None;
                }

                // parse the content and update Q&As
                let document = Html::parse_document(&content);
                static QNAS: OnceLock<Selector> = OnceLock::new();
                let qnas = QNAS.get_or_init(|| Selector::parse(".qa-List_Item").unwrap());
                static QNA_NUMBER: OnceLock<Selector> = OnceLock::new();
                let qna_number =
                    QNA_NUMBER.get_or_init(|| Selector::parse(".qa-List_Ttl").unwrap());

                let mut page_updated_count = 0;

                for hololive_qna in document.select(qnas) {
                    let Some(mut qna_number) = hololive_qna
                        .select(qna_number)
                        .next()
                        .map(|c| c.text().collect::<String>())
                    else {
                        println!("Q&A number not found");
                        return None;
                    };

                    // e.g. Q398 (2025.06.11)
                    let mut date = None;
                    if let Some((number, date_str)) = qna_number.split_once(' ') {
                        date = Some(
                            date_str
                                .trim()
                                .trim_start_matches('(')
                                .trim_end_matches(')')
                                .replace('.', "-"),
                        );
                        qna_number = number.into();
                    }

                    // Find the Q&A or create
                    let mut all_qnas = all_qnas.write();
                    let qna = all_qnas.entry(qna_number.clone().into()).or_default();

                    // update Q&A information
                    qna.qna_number = qna_number;
                    qna.date = date;
                    update_qna(qna, &hololive_qna, language);

                    page_updated_count += 1;
                }

                // Increment the total updated count
                *updated_count.lock() += page_updated_count;

                println!("Page {page} done: updated {page_updated_count} Q&As");

                Some(())
            }
        })
        .while_some()
        .max(); // Need this to drive the iterator

    println!("Updated {} Q&As", *updated_count.lock());
}

fn update_qna(qna: &mut Qna, hololive_qna: &scraper::ElementRef, language: Language) {
    static QUESTION: OnceLock<Selector> = OnceLock::new();
    let question = QUESTION.get_or_init(|| Selector::parse(".qa-List_Txt-Q").unwrap());

    // Question
    let question = hololive_qna
        .select(question)
        .next()
        .map(|c| c.text().collect::<String>())
        .map(|s| s.trim_start_matches('Q').into());
    *qna.question.value_mut(language) = question;

    static ANSWER: OnceLock<Selector> = OnceLock::new();
    let answer = ANSWER.get_or_init(|| Selector::parse(".qa-List_Txt-A").unwrap());

    // Answer
    let answer = hololive_qna
        .select(answer)
        .next()
        .map(|c| c.text().collect::<String>())
        .map(|s| s.trim_start_matches('A').into());
    *qna.answer.value_mut(language) = answer;

    static RELATION: OnceLock<Selector> = OnceLock::new();
    let info = RELATION.get_or_init(|| Selector::parse(".relation").unwrap());

    // Relation
    let mut relations = hololive_qna
        .select(info)
        .next()
        .map(|c| c.text().collect::<String>())
        .into_iter()
        .flat_map(|s| {
            s.lines()
                .skip(1) // skip 関連カード
                .map(|line| line.trim().trim_start_matches('['))
                .filter_map(|line| line.split_once('：'))
                .map(|(card_number, _)| card_number.trim().to_string())
                .filter(|line| !line.is_empty())
                .collect_vec()
        })
        .collect_vec();
    relations.sort();
    qna.referenced_cards = relations;
}
