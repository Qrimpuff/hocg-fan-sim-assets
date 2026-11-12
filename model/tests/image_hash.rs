use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

use hocg_fan_sim_assets_model::{
    CardIllustration, CardRarity, CardsDatabase, Language,
    img_hash::{
        DIST_TOLERANCE_DIFF_RARITY, DIST_TOLERANCE_SAME_RARITY, dist_hash, hash_image_language,
        is_similar, path_to_image_hash,
    },
};

#[ignore]
#[test]
// good way to visualize the differences in image hashes
// hashes are computed from the images, so they always match the current hash version
fn test_image_hash_from_json() {
    let assets_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("assets");
    let cards_json_path = assets_path.join("hocg_cards.json");
    let s = fs::read_to_string(&cards_json_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", cards_json_path.display(), e));
    let all_cards: CardsDatabase = serde_json::from_str(&s).unwrap();

    let illusts = all_cards
        .values()
        .flat_map(|cs| cs.illustrations.iter())
        .collect::<Vec<_>>();

    let hashes = illusts
        .iter()
        .map(|illust| {
            let (path, language) = match (
                illust.img_path.value(Language::Japanese),
                illust.img_path.value(Language::English),
            ) {
                (Some(p), _) => (assets_path.join("img").join(p), Language::Japanese),
                (None, Some(p)) => (assets_path.join("img_en").join(p), Language::English),
                _ => return None,
            };
            path.is_file()
                .then(|| path_to_image_hash(&path, Some(language)))
        })
        .collect::<Vec<_>>();

    let display_id = |idx: usize| {
        illusts[idx]
            .manage_id
            .japanese
            .iter()
            .flatten()
            .next()
            .map(|id| format!("JP-{}", id))
            .or_else(|| {
                illusts[idx]
                    .manage_id
                    .english
                    .iter()
                    .flatten()
                    .next()
                    .map(|id| format!("EN-{}", id))
            })
            .or_else(|| {
                illusts[idx]
                    .ogbajoj_sheet_cells
                    .iter()
                    .flatten()
                    .next()
                    .map(|(sheet_id, cell)| format!("{}:{}", sheet_id, cell))
            })
            .unwrap_or_else(|| format!("#{}", idx + 1))
    };

    // cross-card check: different cards should never look similar
    let mut pair_count = 0usize;
    let mut min_dist: Option<(u64, usize, usize)> = None;
    let mut close_pairs = Vec::new();

    for i in 0..illusts.len() {
        for j in (i + 1)..illusts.len() {
            let (Some(h1), Some(h2)) = (&hashes[i], &hashes[j]) else {
                continue;
            };
            if illusts[i].card_number == illusts[j].card_number {
                continue;
            }
            pair_count += 1;

            let dist = dist_hash(h1, h2);
            if min_dist.is_none_or(|(min, _, _)| dist < min) {
                min_dist = Some((dist, i, j));
            }
            // same tolerance rule as is_similar
            let tolerance = if illusts[i].rarity == illusts[j].rarity {
                DIST_TOLERANCE_SAME_RARITY
            } else {
                DIST_TOLERANCE_DIFF_RARITY
            };
            if dist <= tolerance {
                close_pairs.push((i, j, dist, tolerance));
            }
        }
    }

    close_pairs.sort_by_key(|(_, _, dist, _)| *dist);

    println!("---------------------------");

    for (i, j, dist, tolerance) in &close_pairs {
        println!(
            "close:\t{}\t{}\t({})\t<->\t{}\t{}\t({})\t=\t{}\ttolerance={}",
            illusts[*i].card_number,
            illusts[*i].rarity,
            display_id(*i),
            illusts[*j].card_number,
            illusts[*j].rarity,
            display_id(*j),
            dist,
            tolerance
        );
    }

    let min_pair = min_dist.map(|(dist, i, j)| {
        format!(
            "{} {} <-> {} {} = {}",
            illusts[i].card_number,
            illusts[i].rarity,
            illusts[j].card_number,
            illusts[j].rarity,
            dist
        )
    });
    println!(
        "cross_card_pairs={} too_close={} min_cross_card={:?}",
        pair_count,
        close_pairs.len(),
        min_pair
    );
}

#[ignore]
#[test]
// within-card grouping must match the stored similarity_index in hocg_cards.json
fn test_similarity_index_matches_stored() {
    let assets_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("assets");
    let cards_json_path = assets_path.join("hocg_cards.json");
    let s = fs::read_to_string(&cards_json_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", cards_json_path.display(), e));
    let mut all_cards: CardsDatabase = serde_json::from_str(&s).unwrap();

    // recompute from images so the check reflects the current hash version
    for illust in all_cards
        .values_mut()
        .flat_map(|cs| cs.illustrations.iter_mut())
    {
        // same as --rebuild-image-similarity: unreleased illustrations keep their hash
        if hash_image_language(illust).is_some() {
            illust.img_hash = image_hash_of(&assets_path, illust).unwrap_or_default();
        }
    }

    // rebuild groups the same way update_similarity_index does, then compare partitions
    let mut pair_count = 0usize;
    let mut mismatches = Vec::new();
    for card in all_cards.values() {
        let ills = &card.illustrations;
        let mut groups = vec![0u32; ills.len()];
        let mut next_group = 1u32;
        for i in 0..ills.len() {
            if ills[i].img_hash.is_empty() {
                continue;
            }
            for j in 0..i {
                if groups[j] != 0 && is_similar(&ills[i], &ills[j]) {
                    groups[i] = groups[j];
                    break;
                }
            }
            if groups[i] == 0 {
                groups[i] = next_group;
                next_group += 1;
            }
        }

        for i in 0..ills.len() {
            for j in (i + 1)..ills.len() {
                let (a, b) = (&ills[i], &ills[j]);
                if a.img_hash.is_empty() || b.img_hash.is_empty() {
                    continue;
                }
                pair_count += 1;
                let stored_same = a.similarity_index == b.similarity_index;
                let rebuilt_same = groups[i] == groups[j];
                if stored_same != rebuilt_same {
                    mismatches.push(format!(
                        "{}\t{}\tsi={}/{}\trebuilt_same={}\tdist={}\t{}\t{}",
                        a.card_number,
                        a.rarity,
                        a.similarity_index,
                        b.similarity_index,
                        rebuilt_same,
                        dist_hash(&a.img_hash, &b.img_hash),
                        i,
                        j
                    ));
                }
            }
        }
    }

    for m in &mismatches {
        println!("mismatch:\t{m}");
    }
    println!(
        "within_card_pairs={} mismatches={}",
        pair_count,
        mismatches.len()
    );
}

fn image_hash_of(assets_path: &Path, illust: &CardIllustration) -> Option<String> {
    let language = hash_image_language(illust)?;
    let folder = match language {
        Language::Japanese => "img",
        Language::English => "img_en",
    };
    let path = assets_path
        .join(folder)
        .join(illust.img_path.value(language).as_deref()?);
    path.is_file()
        .then(|| path_to_image_hash(&path, Some(language)))
}

// Unofficial images: "SAMPLE" watermarks, other crops and scans. They have no manage_id.
const UNRELEASED_FIXTURES: &[&str] = &[
    "hBP01-117_C_b.webp",
    "hBP01-120_C_b.webp",
    "hBP04-005_OSR_b.webp",
    "hBP03-089_U_b.webp",
    "hBP01-056_C_b.webp",
    "hBD24-001_P_b.webp",
    "hBP04-009_P_b.webp",
];

fn fixture_illustration(card_number: &str, file: &str, rarity: CardRarity) -> CardIllustration {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("card_images")
        .join(file);
    let language = if file.starts_with("EN_") {
        Language::English
    } else {
        Language::Japanese
    };
    let mut illustration = CardIllustration {
        card_number: card_number.to_owned(),
        rarity,
        img_hash: path_to_image_hash(&path, Some(language)),
        ..Default::default()
    };
    if !UNRELEASED_FIXTURES.contains(&file) {
        *illustration.manage_id.value_mut(language) = Some(BTreeSet::from([1]));
    }
    illustration
}

// a macro to build a test for comparing two images
// is_similar must give the expected answer; images of the same illustration must also
// stay within the dist_hash tolerance used to match images from other sources
macro_rules! image_hash_tests {
    ($test_name:ident, $card_number1:expr, $path1:expr, $card_number2:expr, $path2:expr, $same_rarity:expr, $expected_similar:expr) => {
        #[test]
        fn $test_name() {
            let a = fixture_illustration($card_number1, $path1, CardRarity::Common);
            let b = fixture_illustration(
                $card_number2,
                $path2,
                if $same_rarity {
                    CardRarity::Common
                } else {
                    CardRarity::Promo
                },
            );
            let dist = dist_hash(&a.img_hash, &b.img_hash);
            assert_eq!(
                is_similar(&a, &b),
                $expected_similar,
                "is_similar (distance {dist})"
            );
            if $expected_similar {
                let tolerance = if $same_rarity {
                    DIST_TOLERANCE_SAME_RARITY
                } else {
                    DIST_TOLERANCE_DIFF_RARITY
                };
                // keep 10% of the tolerance as margin
                assert!(
                    dist <= tolerance * 9 / 10,
                    "distance {dist} is too close to the tolerance {tolerance}"
                );
            }
        }
    };
}

image_hash_tests!(
    test_hbp01_007_our_vs_sec,
    "hBP01-007",
    "hBP01-007_OUR_a.webp",
    "hBP01-007",
    "hBP01-007_SEC_b.webp",
    false,
    false
);

image_hash_tests!(
    test_hbp01_007_our_vs_sec_en,
    "hBP01-007",
    "EN_hBP01-007_OUR_c.webp",
    "hBP01-007",
    "EN_hBP01-007_SEC_d.webp",
    false,
    false
);

image_hash_tests!(
    test_hbp01_007_jp_vs_en_our,
    "hBP01-007",
    "hBP01-007_OUR_a.webp",
    "hBP01-007",
    "EN_hBP01-007_OUR_c.webp",
    true,
    true
);

image_hash_tests!(
    test_hbp01_007_jp_vs_en_sec,
    "hBP01-007",
    "hBP01-007_SEC_b.webp",
    "hBP01-007",
    "EN_hBP01-007_SEC_d.webp",
    true,
    true
);

image_hash_tests!(
    test_hbp01_007_champion_vs_top4,
    "hBP01-007",
    "hBP01-007_P_a.webp",
    "hBP01-007",
    "hBP01-007_P_b.webp",
    true,
    false
);

image_hash_tests!(
    test_hbp01_117_c_sample,
    "hBP01-117",
    "hBP01-117_C_a.webp",
    "hBP01-117",
    "hBP01-117_C_b.webp",
    true,
    true
);

image_hash_tests!(
    test_hbp01_120_c_sample,
    "hBP01-120",
    "hBP01-120_C_a.webp",
    "hBP01-120",
    "hBP01-120_C_b.webp",
    true,
    true
);

image_hash_tests!(
    test_hbp02_008_c_variants,
    "hBP02-008",
    "hBP02-008_C_a.webp",
    "hBP02-008",
    "hBP02-008_C_b.webp",
    true,
    false
);

image_hash_tests!(
    test_hbp02_008_c_vs_p_similar,
    "hBP02-008",
    "hBP02-008_C_a.webp",
    "hBP02-008",
    "hBP02-008_P_2_c.webp",
    false,
    true
);

image_hash_tests!(
    test_hbp04_005_osr_sample,
    "hBP04-005",
    "hBP04-005_OSR_a.webp",
    "hBP04-005",
    "hBP04-005_OSR_b.webp",
    true,
    true
);

image_hash_tests!(
    test_hbp04_009_p_scan,
    "hBP04-009",
    "hBP04-009_P_02_a.webp",
    "hBP04-009",
    "hBP04-009_P_b.webp",
    true,
    true
);

image_hash_tests!(
    test_hsd05_002_c_vs_p_similar,
    "hSD05-002",
    "hSD05-002_C_a.webp",
    "hSD05-002",
    "hSD05-002_P_b.webp",
    false,
    true
);

image_hash_tests!(
    test_hy04_vs_hy05_different_cards,
    "hY04-001",
    "hY04-001_C_a.webp",
    "hY05-001",
    "hY05-001_C_b.webp",
    false,
    false
);

image_hash_tests!(
    test_hbp01_117_vs_120_different_cards,
    "hBP01-117",
    "hBP01-117_C_a.webp",
    "hBP01-120",
    "hBP01-120_C_a.webp",
    false,
    false
);

image_hash_tests!(
    test_hbp04_037_rr_vs_sr,
    "hBP04-037",
    "hBP04-037_RR_a.webp",
    "hBP04-037",
    "hBP04-037_SR_b.webp",
    false,
    false
);

image_hash_tests!(
    test_hbp01_056_c_crop,
    "hBP01-056",
    "hBP01-056_C_a.webp",
    "hBP01-056",
    "hBP01-056_C_b.webp",
    true,
    true
);

image_hash_tests!(
    test_hbp03_024_rr_vs_sr,
    "hBP03-024",
    "hBP03-024_RR_a.webp",
    "hBP03-024",
    "hBP03-024_SR_b.webp",
    false,
    false
);

image_hash_tests!(
    test_hbp03_036_r_vs_sr,
    "hBP03-036",
    "hBP03-036_R_a.webp",
    "hBP03-036",
    "hBP03-036_SR_b.webp",
    false,
    false
);

image_hash_tests!(
    test_hbp03_089_u_sample,
    "hBP03-089",
    "hBP03-089_U_a.webp",
    "hBP03-089",
    "hBP03-089_U_b.webp",
    true,
    true
);

image_hash_tests!(
    test_hbp03_037_c_different,
    "hBP03-037",
    "hBP03-037_C_a.webp",
    "hBP03-037",
    "hBP03-037_C_b.webp",
    true,
    false
);

image_hash_tests!(
    test_hbp03_040_c_different,
    "hBP03-040",
    "hBP03-040_C_a.webp",
    "hBP03-040",
    "hBP03-040_C_b.webp",
    true,
    false
);

image_hash_tests!(
    test_hbp01_088_c_different,
    "hBP01-088",
    "hBP01-088_C_a.webp",
    "hBP01-088",
    "hBP01-088_C_b.webp",
    true,
    false
);

image_hash_tests!(
    test_hy01_001_c_vs_s_similar,
    "hY01-001",
    "hY01-001_C.webp",
    "hY01-001",
    "hY01-001_S.webp",
    false,
    true
);

image_hash_tests!(
    test_hbp01_095_sr_vs_p_similar,
    "hBP01-095",
    "EN_hBP01-095_SR.webp",
    "hBP01-095",
    "hBP01-095_P.webp",
    false,
    true
);

image_hash_tests!(
    test_hbp09_105_u_vs_p_different,
    "hBP09-105",
    "hBP09-105_U.webp",
    "hBP09-105",
    "hBP09-105_P.webp",
    false,
    false
);

image_hash_tests!(
    test_hbp05_080_u_vs_p_different,
    "hBP05-080",
    "hBP05-080_U.webp",
    "hBP05-080",
    "hBP05-080_P.webp",
    false,
    false
);

image_hash_tests!(
    test_hbp02_046_r_vs_sr,
    "hBP02-046",
    "hBP02-046_R_a.webp",
    "hBP02-046",
    "hBP02-046_SR_b.webp",
    false,
    false
);

image_hash_tests!(
    test_hbp05_074_c_variants,
    "hBP05-074",
    "hBP05-074_C_a.webp",
    "hBP05-074",
    "hBP05-074_C_b.webp",
    true,
    false
);

image_hash_tests!(
    test_hbp01_104_c_variants_1,
    "hBP01-104",
    "hBP01-104_C_a.webp",
    "hBP01-104",
    "hBP01-104_C_b.webp",
    true,
    false
);

image_hash_tests!(
    test_hbp01_104_c_variants_2,
    "hBP01-104",
    "hBP01-104_C_b.webp",
    "hBP01-104",
    "hBP01-104_C_c.webp",
    true,
    false
);

image_hash_tests!(
    test_hbp01_104_c_variants_3,
    "hBP01-104",
    "hBP01-104_C_c.webp",
    "hBP01-104",
    "hBP01-104_C_a.webp",
    true,
    false
);

image_hash_tests!(
    test_hbp01_104_c_variants_4,
    "hBP01-104",
    "hBP01-104_C_d.webp",
    "hBP01-104",
    "hBP01-104_C_e.webp",
    true,
    false
);

image_hash_tests!(
    test_hbp01_104_c_variants_5,
    "hBP01-104",
    "hBP01-104_C_a.webp",
    "hBP01-104",
    "hBP01-104_C_f.webp",
    true,
    false
);

image_hash_tests!(
    test_hbd24_001_p_crop,
    "hBD24-001",
    "hBD24-001_P_a.webp",
    "hBD24-001",
    "hBD24-001_P_b.webp",
    true,
    true
);
