use std::fs;
use std::ops::Mul;
use std::path::Path;
use std::path::PathBuf;

use hocg_fan_sim_assets_cli::images::utils::{
    DIST_TOLERANCE_DIFF_RARITY, dist_hash, path_to_image_hash,
};
use hocg_fan_sim_assets_model::CardsDatabase;

#[allow(dead_code)]
// good way to visualize the differences in image hashes
fn test_image_hash_from_json() {
    let s = fs::read_to_string("assets/hocg_cards.json").unwrap();
    let all_cards: CardsDatabase = serde_json::from_str(&s).unwrap();

    let illusts = all_cards
        .values()
        .flat_map(|cs| cs.illustrations.iter())
        .collect::<Vec<_>>();

    dbg!(illusts.len());

    let mut results = Vec::with_capacity((illusts.len() * illusts.len()) / 2);

    for i in 0..illusts.len() {
        for j in (i + 1)..illusts.len() {
            if illusts[i].img_hash != illusts[j].img_hash {
                // if illusts[i].card_number.starts_with("hY")
                //     && illusts[i].card_number.ends_with("001")
                //     && illusts[j].card_number.starts_with("hY")
                //     && illusts[j].card_number.ends_with("001")
                if illusts[i].card_number == illusts[j].card_number
                // && illusts[i].rarity == illusts[j].rarity
                {
                    let dist = dist_hash(&illusts[i].img_hash, &illusts[j].img_hash);
                    results.push((i, j, dist));
                    println!(
                        "dist:\t{}\t{}\t({})\t<->\t{}\t{}\t({})\t=\t{}",
                        illusts[i].card_number,
                        illusts[i].rarity,
                        illusts[i]
                            .manage_id
                            .japanese
                            .iter()
                            .flatten()
                            .next()
                            .map(|id| format!("JP-{}", id))
                            .or_else(|| illusts[i]
                                .manage_id
                                .english
                                .iter()
                                .flatten()
                                .next()
                                .map(|id| format!("EN-{}", id)))
                            .unwrap_or("<?>".into()),
                        illusts[j].card_number,
                        illusts[j].rarity,
                        illusts[j]
                            .manage_id
                            .japanese
                            .iter()
                            .flatten()
                            .next()
                            .map(|id| format!("JP-{}", id))
                            .or_else(|| illusts[j]
                                .manage_id
                                .english
                                .iter()
                                .flatten()
                                .next()
                                .map(|id| format!("EN-{}", id)))
                            .unwrap_or("<?>".into()),
                        dist
                    );
                }
            }
        }
    }

    results.sort_by_key(|(_, _, dist)| *dist);

    println!("---------------------------");

    for (i, j, dist) in results {
        println!(
            "sorted:\t{}\t{}\t({})\t<->\t{}\t{}\t({})\t=\t{}",
            illusts[i].card_number,
            illusts[i].rarity,
            illusts[i]
                .manage_id
                .japanese
                .iter()
                .flatten()
                .next()
                .map(|id| format!("JP-{}", id))
                .or_else(|| illusts[i]
                    .manage_id
                    .english
                    .iter()
                    .flatten()
                    .next()
                    .map(|id| format!("EN-{}", id)))
                .unwrap_or("<?>".into()),
            illusts[j].card_number,
            illusts[j].rarity,
            illusts[j]
                .manage_id
                .japanese
                .iter()
                .flatten()
                .next()
                .map(|id| format!("JP-{}", id))
                .or_else(|| illusts[j]
                    .manage_id
                    .english
                    .iter()
                    .flatten()
                    .next()
                    .map(|id| format!("EN-{}", id)))
                .unwrap_or("<?>".into()),
            dist
        );
    }
}

fn image_is_similar(path_1: &Path, path_2: &Path, nudge: i64) -> bool {
    let hash_1 = path_to_image_hash(path_1);
    let hash_2 = path_to_image_hash(path_2);

    dist_hash(&hash_1, &hash_2) <= (DIST_TOLERANCE_DIFF_RARITY as i64 + nudge) as u64
}

// a macro to build a test for comparing two images
macro_rules! image_hash_tests {
    ($test_name:ident, $path1:expr, $path2:expr, $expected_similar:expr) => {
        #[test]
        fn $test_name() {
            let mut d = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            d.push("tests/card_images");
            let path1 = d.join($path1);
            let path2 = d.join($path2);
            let similar = image_is_similar(
                &path1,
                &path2,
                (DIST_TOLERANCE_DIFF_RARITY as i64 / 10).mul(if $expected_similar {
                    -1
                } else {
                    1
                }),
            );
            assert_eq!(
                similar,
                $expected_similar,
                "Expected images to be {}, but got {}",
                if $expected_similar {
                    "similar"
                } else {
                    "different"
                },
                if similar { "similar" } else { "different" }
            );
        }
    };
}

image_hash_tests!(
    test_hbp01_007_our_vs_sec,
    "hBP01-007_OUR_a.webp",
    "hBP01-007_SEC_b.webp",
    false
);

image_hash_tests!(
    test_hbp01_007_our_vs_sec_en,
    "EN_hBP01-007_OUR_c.webp",
    "EN_hBP01-007_SEC_d.webp",
    false
);

image_hash_tests!(
    test_hbp01_007_jp_vs_en_our,
    "hBP01-007_OUR_a.webp",
    "EN_hBP01-007_OUR_c.webp",
    true
);

image_hash_tests!(
    test_hbp01_007_jp_vs_en_sec,
    "hBP01-007_SEC_b.webp",
    "EN_hBP01-007_SEC_d.webp",
    true
);

image_hash_tests!(
    test_hbp01_117_c_sample,
    "hBP01-117_C_a.webp",
    "hBP01-117_C_b.webp",
    true
);

image_hash_tests!(
    test_hbp01_120_c_sample,
    "hBP01-120_C_a.webp",
    "hBP01-120_C_b.webp",
    true
);

image_hash_tests!(
    test_hbp02_008_c_variants,
    "hBP02-008_C_a.webp",
    "hBP02-008_C_b.webp",
    false
);

image_hash_tests!(
    test_hbp02_008_c_vs_p_similar,
    "hBP02-008_C_a.webp",
    "hBP02-008_P_2_c.webp",
    true
);

image_hash_tests!(
    test_hbp04_005_osr_sample,
    "hBP04-005_OSR_a.webp",
    "hBP04-005_OSR_b.webp",
    true
);

image_hash_tests!(
    test_hbp04_009_p_scan,
    "hBP04-009_P_02_a.webp",
    "hBP04-009_P_b.webp",
    true
);

image_hash_tests!(
    test_hsd05_002_c_vs_p_similar,
    "hSD05-002_C_a.webp",
    "hSD05-002_P_b.webp",
    true
);

image_hash_tests!(
    test_hy04_vs_hy05_different_cards,
    "hY04-001_C_a.webp",
    "hY05-001_C_b.webp",
    false
);

image_hash_tests!(
    test_hbp01_117_vs_120_different_cards,
    "hBP01-117_C_a.webp",
    "hBP01-120_C_a.webp",
    false
);

image_hash_tests!(
    test_hbp04_037_rr_vs_sr,
    "hBP04-037_RR_a.webp",
    "hBP04-037_SR_b.webp",
    false
);

image_hash_tests!(
    test_hbp01_056_c_crop,
    "hBP01-056_C_a.webp",
    "hBP01-056_C_b.webp",
    true
);

image_hash_tests!(
    test_hbp03_024_rr_vs_sr,
    "hBP03-024_RR_a.webp",
    "hBP03-024_SR_b.webp",
    false
);

image_hash_tests!(
    test_hbp03_036_r_vs_sr,
    "hBP03-036_R_a.webp",
    "hBP03-036_SR_b.webp",
    false
);

image_hash_tests!(
    test_hbp03_089_u_sample,
    "hBP03-089_U_a.webp",
    "hBP03-089_U_b.webp",
    true
);

image_hash_tests!(
    test_hbp02_046_r_vs_sr,
    "hBP02-046_R_a.webp",
    "hBP02-046_SR_b.webp",
    false
);
