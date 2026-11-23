//! T1.6: the committed golden RNG files (RNG-9, T-RNG-1, T-RNG-6, T-RNG-7).
//!
//! The two files are generated once by `examples/gen_golden.rs`, reviewed by a
//! human and committed (PLAN 6.4). They are read from disk at run time, so any
//! later change to either file fails here and therefore fails CI.

use rand_core::RngCore;
use superminigames_core::rng::{chacha_from_seed, uniform_index};

/// The registry sorted as `[A, B, C, D, E]` (T-RNG-1).
const POOL: [&str; 5] = ["A", "B", "C", "D", "E"];

/// T-RNG-1, T-RNG-6 and T-RNG-7 all use seed 0.
const SEED: u64 = 0;

/// The `key=value` rows of a golden file, comments and blank lines dropped.
fn golden(name: &str) -> Vec<(String, String)> {
    let path = format!("{}/tests/{name}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("RNG-9: cannot read {path}: {error}"));
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let (key, value) = line
                .split_once('=')
                .unwrap_or_else(|| panic!("RNG-9: {name} row is not key=value: {line}"));
            (key.to_owned(), value.to_owned())
        })
        .collect()
}

/// The round index of a `round_<k>` row, or `None` for any other row.
fn round_of(key: &str) -> Option<u64> {
    key.strip_prefix("round_").map(|index| {
        index
            .parse()
            .expect("RNG-9: a round_ key holds a decimal index")
    })
}

#[test]
fn t_rng_1_golden_selection_matches_seed_zero() {
    // REQ: RNG-1, RNG-3, RNG-4, T-RNG-1
    let mut checked = 0;
    for (key, expected) in golden("golden_seed0.txt") {
        let Some(round) = round_of(&key) else {
            continue;
        };
        assert!(
            (0..=9).contains(&round),
            "T-RNG-1: rounds 0..=9, saw {round}"
        );
        let mut rng = chacha_from_seed(SEED, round);
        let pick = POOL[uniform_index(&mut rng, POOL.len())];
        assert_eq!(pick, expected, "T-RNG-1: seed 0 round {round}");
        checked += 1;
    }
    assert_eq!(checked, 10, "T-RNG-1: the file holds rounds 0..=9");
}

#[test]
fn t_rng_7_golden_opening_bit_matches_seed_zero() {
    // REQ: RNG-11, T-RNG-7
    let committed = golden("golden_seed0.txt")
        .into_iter()
        .find(|(key, _)| key == "opening_bit")
        .map(|(_, value)| value)
        .expect("T-RNG-7: golden_seed0.txt holds an opening_bit row");
    let opening = chacha_from_seed(SEED, 1 << 63).next_u64() & 1;
    assert_eq!(opening.to_string(), committed, "T-RNG-7: seed 0");
}

#[test]
fn t_rng_6_golden_game_seeds_match_seed_zero() {
    // REQ: RNG-10, T-RNG-6
    let mut checked = 0;
    for (key, expected) in golden("golden_game_seed0.txt") {
        let Some(round) = round_of(&key) else {
            continue;
        };
        assert!(
            (0..=4).contains(&round),
            "T-RNG-6: rounds 0..=4, saw {round}"
        );
        let game_seed = chacha_from_seed(SEED, u64::MAX - round).next_u64();
        assert_eq!(
            game_seed.to_string(),
            expected,
            "T-RNG-6: seed 0 round {round}"
        );
        checked += 1;
    }
    assert_eq!(checked, 5, "T-RNG-6: the file holds rounds 0..=4");
}
