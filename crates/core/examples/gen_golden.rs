//! One-off generator for the golden RNG files (T1.6, PLAN 6.4).
//!
//! Run once from the repo root:
//!
//! ```text
//! cargo run -p superminigames-core --example gen_golden
//! ```
//!
//! Writes `crates/core/tests/golden_seed0.txt` and
//! `crates/core/tests/golden_game_seed0.txt`. The files are then human-reviewed
//! and committed; `crates/core/tests/golden.rs` fails in CI if either changes
//! (RNG-9). This example only *produces* values, it asserts nothing.

use rand_core::RngCore;
use superminigames_core::rng::{chacha_from_seed, uniform_index};

/// The registry sorted as `[A, B, C, D, E]` (T-RNG-1).
const POOL: [&str; 5] = ["A", "B", "C", "D", "E"];

/// T-RNG-1, T-RNG-6 and T-RNG-7 all use seed 0.
const SEED: u64 = 0;

fn main() -> std::io::Result<()> {
    let mut selection = vec![
        "# Golden selection for seed 0 (RNG-9, T-RNG-1, T-RNG-7).".to_owned(),
        "# Pool is the registry sorted as [A,B,C,D,E] and each round makes exactly".to_owned(),
        "# one draw: pool[uniform_index(&mut chacha_from_seed(0, k), 5)] (RNG-3, RNG-4)."
            .to_owned(),
        "# opening_bit is chacha_from_seed(0, 1<<63).next_u64() & 1 (RNG-11).".to_owned(),
        format!(
            "opening_bit={}",
            chacha_from_seed(SEED, 1 << 63).next_u64() & 1
        ),
    ];
    for round in 0..=9u64 {
        let mut rng = chacha_from_seed(SEED, round);
        selection.push(format!(
            "round_{round}={}",
            POOL[uniform_index(&mut rng, POOL.len())]
        ));
    }

    let mut game_seeds = vec![
        "# Golden game seeds for seed 0 (RNG-9, RNG-10, T-RNG-6).".to_owned(),
        "# game_seed(k) is the first next_u64() of chacha_from_seed(0, u64::MAX - k).".to_owned(),
    ];
    for round in 0..=4u64 {
        let game_seed = chacha_from_seed(SEED, u64::MAX - round).next_u64();
        game_seeds.push(format!("round_{round}={game_seed}"));
    }

    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests");
    std::fs::write(format!("{dir}/golden_seed0.txt"), join(&selection))?;
    std::fs::write(format!("{dir}/golden_game_seed0.txt"), join(&game_seeds))?;
    Ok(())
}

/// One trailing newline, so the files are stable under ordinary tooling.
fn join(lines: &[String]) -> String {
    let mut text = lines.join("\n");
    text.push('\n');
    text
}
