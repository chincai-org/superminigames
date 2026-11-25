//! T1.5: RNG helper properties (RNG-2, RNG-3, RNG-5, T-RNG-2) and the scoring
//! adapters' SCR-2/SCR-3 mapping.

use rand_chacha::ChaCha8Rng;
use rand_core::{RngCore, SeedableRng};
use superminigames_core::outcome::{DisqualificationReason, PlayerOutcome};
use superminigames_core::rng::{chacha_from_seed, uniform_index};
use superminigames_core::scoring::{POINTS_SCALE, compute_places, key_of, points_for_place};

#[test]
fn t_rng_2_seed_zero_is_the_all_zero_key() {
    // REQ: RNG-2
    let mut helper = chacha_from_seed(0, 0);
    let mut reference = ChaCha8Rng::from_seed([0u8; 32]);
    assert_eq!(helper.next_u64(), reference.next_u64());
}

#[test]
fn t_rng_2_the_key_holds_the_seed_little_endian_in_the_first_eight_bytes() {
    // REQ: RNG-2
    for seed in [1u64, 0x0102_0304_0506_0708, u64::MAX] {
        let mut key = [0u8; 32];
        key[..8].copy_from_slice(&seed.to_le_bytes());
        let mut helper = chacha_from_seed(seed, 0);
        let mut reference = ChaCha8Rng::from_seed(key);
        assert_eq!(helper.next_u64(), reference.next_u64(), "seed {seed}");
    }
}

#[test]
fn t_rng_3_the_stream_comes_from_the_round_index() {
    // REQ: RNG-3
    // The round's pick depends only on (seed, k, pool), so round 5 is the same
    // whatever happened in rounds 0..4.
    let round_five: Vec<u64> = (0..3).map(|_| chacha_from_seed(0, 5).next_u64()).collect();
    assert_eq!(round_five[0], round_five[1]);
    assert_eq!(round_five[1], round_five[2]);

    let other_stream = chacha_from_seed(0, 6).next_u64();
    assert_ne!(round_five[0], other_stream);
}

#[test]
fn t_rng_5_uniform_index_stays_inside_the_range() {
    // REQ: RNG-5
    for n in 1usize..=8 {
        let mut rng = chacha_from_seed(0, n as u64);
        for _ in 0..1_000 {
            assert!(uniform_index(&mut rng, n) < n);
        }
    }
}

#[test]
fn t_rng_2_uniformity_is_within_five_percent_for_a_three_game_pool() {
    // REQ: RNG-5, T-RNG-2
    let mut counts = [0u32; 3];
    for round_index in 0..30_000u64 {
        let mut rng = chacha_from_seed(0, round_index);
        counts[uniform_index(&mut rng, 3)] += 1;
    }
    for (game, count) in counts.iter().enumerate() {
        assert!(
            count.abs_diff(10_000) <= 500,
            "game {game} was picked {count} times, outside 5% of 10000"
        );
    }
    assert_eq!(counts.iter().sum::<u32>(), 30_000);
}

#[test]
fn t_rng_5_uniform_index_is_deterministic() {
    // REQ: RNG-5 — no flake: the same seed and stream give the same index.
    let first: Vec<usize> = (0..64)
        .map(|round_index| {
            let mut rng = chacha_from_seed(42, round_index);
            uniform_index(&mut rng, 5)
        })
        .collect();
    let second: Vec<usize> = (0..64)
        .map(|round_index| {
            let mut rng = chacha_from_seed(42, round_index);
            uniform_index(&mut rng, 5)
        })
        .collect();
    assert_eq!(first, second);
}

#[test]
fn t_scr_1_points_scale_is_twelve() {
    // REQ: SCR-6
    assert_eq!(POINTS_SCALE, 12);
}

#[test]
fn t_scr_3_keys_follow_the_scrapbook_table() {
    // REQ: SCR-3
    assert_eq!(key_of(PlayerOutcome::Placed(1)), (0, 1));
    assert_eq!(key_of(PlayerOutcome::Scored(12)), (0, -12));
    assert_eq!(key_of(PlayerOutcome::Scored(-3)), (0, 3));
    assert_eq!(
        key_of(PlayerOutcome::NoResult(
            DisqualificationReason::TimerExpired
        )),
        (1, 0)
    );
    assert_eq!(
        key_of(PlayerOutcome::NoResult(DisqualificationReason::LeftRound)),
        (1, 0)
    );
}

#[test]
fn t_scr_3_higher_scores_rank_first() {
    // REQ: SCR-3, SCR-1
    let outcomes = [
        PlayerOutcome::Scored(12),
        PlayerOutcome::Scored(11),
        PlayerOutcome::Scored(3),
        PlayerOutcome::Scored(0),
    ];
    assert_eq!(
        compute_places(&outcomes),
        vec![(1, 1), (2, 1), (3, 1), (4, 1)]
    );
}

#[test]
fn t_scr_3_tied_scores_share_a_place_and_group() {
    // REQ: SCR-3, SCR-8
    let outcomes = [
        PlayerOutcome::Scored(5),
        PlayerOutcome::Scored(5),
        PlayerOutcome::Scored(0),
        PlayerOutcome::Scored(0),
    ];
    assert_eq!(
        compute_places(&outcomes),
        vec![(1, 2), (1, 2), (3, 2), (3, 2)]
    );
    let points: Vec<u32> = compute_places(&outcomes)
        .into_iter()
        .map(|(place, group)| points_for_place(outcomes.len(), place, group))
        .collect();
    assert_eq!(points, vec![10, 10, 2, 2]);
}

#[test]
fn t_scr_5_missing_results_rank_below_every_outcome() {
    // REQ: SCR-5, T-SCR-4
    let outcomes = [
        PlayerOutcome::Placed(1),
        PlayerOutcome::Placed(2),
        PlayerOutcome::NoResult(DisqualificationReason::LeftRound),
        PlayerOutcome::NoResult(DisqualificationReason::TimerExpired),
    ];
    assert_eq!(
        compute_places(&outcomes),
        vec![(1, 1), (2, 1), (3, 2), (3, 2)]
    );
    let points: Vec<u32> = compute_places(&outcomes)
        .into_iter()
        .map(|(place, group)| points_for_place(outcomes.len(), place, group))
        .collect();
    assert_eq!(points, vec![12, 8, 2, 2]);
}

#[test]
fn t_scr_2_a_draw_splits_the_points() {
    // REQ: SCR-6, SCR-8
    let outcomes = [PlayerOutcome::Placed(1), PlayerOutcome::Placed(1)];
    assert_eq!(compute_places(&outcomes), vec![(1, 2), (1, 2)]);
    assert_eq!(points_for_place(2, 1, 2), 6);
}
