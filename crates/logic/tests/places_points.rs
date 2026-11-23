//! T-SCR-1, T-SCR-2, T-SCR-3, T-SCR-4, T-SCR-7 and T-SCR-11, plus the SCR-4,
//! SCR-6, SCR-7 and SCR-11 properties of VER-14 for `places_from_keys`,
//! `points_for_place` and `round_points`.
//!
//! VER-19: proptest covers the VER-14 properties until Verus lands in T8.2.
//! Keys are built directly from the SCR-3 table; mapping `PlayerOutcome` to a
//! key is the `core` adapter's job (T1.5), not this crate's.

use proptest::prelude::*;
use superminigames_logic::places::places_from_keys;
use superminigames_logic::points::{points_for_place, round_points, POINTS_SCALE};

/// A `Placed(v)` key (SCR-3).
fn placed(v: u8) -> (u8, i128) {
    (0, i128::from(v))
}

/// A `Scored(s)` key of a HigherIsBetter game: higher scores sort first (SCR-3).
fn scored(s: i128) -> (u8, i128) {
    (0, -s)
}

/// A `NoResult(_)` key, which ranks below every outcome (SCR-3, SCR-5).
fn no_result() -> (u8, i128) {
    (1, 0)
}

/// Base points of place `q` with no tie (SCR-6): `12 * (n - q) / (n - 1)`.
fn base_points(n: usize, q: usize) -> u32 {
    POINTS_SCALE * (n as u32 - q as u32) / (n as u32 - 1)
}

/// Games in this workspace have at most 4 participants (SCR-12).
fn keys_strategy() -> impl Strategy<Value = Vec<(u8, i128)>> {
    (2usize..=4).prop_flat_map(|n| prop::collection::vec(key_strategy(), n))
}

/// SCR-3 key shapes. The value pool is small so that ties and multi-player
/// groups occur often; the `any` case keeps extreme values in play.
fn key_strategy() -> impl Strategy<Value = (u8, i128)> {
    prop_oneof![(0u8..=1, -3i128..=3), (0u8..=1, any::<i128>()),]
}

#[test]
fn t_scr_1_no_ties_points_match_scr_7_table() {
    // REQ: SCR-7
    let table: [&[u32]; 3] = [&[12, 0], &[12, 6, 0], &[12, 8, 4, 0]];
    for (index, expected) in table.iter().enumerate() {
        let n = index + 2;
        let keys: Vec<(u8, i128)> = (1..=n as u8).map(placed).collect();
        assert_eq!(round_points(&keys), expected.to_vec(), "n = {n}");
    }
}

#[test]
fn t_scr_2_n2_draw_splits_points() {
    // REQ: SCR-8
    let keys = [placed(1), placed(1)];
    assert_eq!(places_from_keys(&keys), vec![(1, 2), (1, 2)]);
    assert_eq!(round_points(&keys), vec![6, 6]);
}

#[test]
fn t_scr_2_n4_places_1_1_3_4() {
    // REQ: SCR-8
    let keys = [placed(1), placed(1), placed(3), placed(4)];
    assert_eq!(
        places_from_keys(&keys),
        vec![(1, 2), (1, 2), (3, 1), (4, 1)]
    );
    assert_eq!(round_points(&keys), vec![10, 10, 4, 0]);
}

#[test]
fn t_scr_2_n4_places_1_2_2_4() {
    // REQ: SCR-8
    let keys = [placed(1), placed(2), placed(2), placed(4)];
    assert_eq!(
        places_from_keys(&keys),
        vec![(1, 1), (2, 2), (2, 2), (4, 1)]
    );
    assert_eq!(round_points(&keys), vec![12, 6, 6, 0]);
}

#[test]
fn t_scr_2_n4_places_1_2_3_3() {
    // REQ: SCR-8
    let keys = [placed(1), placed(2), placed(3), placed(3)];
    assert_eq!(
        places_from_keys(&keys),
        vec![(1, 1), (2, 1), (3, 2), (3, 2)]
    );
    assert_eq!(round_points(&keys), vec![12, 8, 2, 2]);
}

#[test]
fn t_scr_2_n4_all_tied() {
    // REQ: SCR-8
    let keys = [placed(1), placed(1), placed(1), placed(1)];
    assert_eq!(
        places_from_keys(&keys),
        vec![(1, 4), (1, 4), (1, 4), (1, 4)]
    );
    assert_eq!(round_points(&keys), vec![6, 6, 6, 6]);
}

#[test]
fn t_scr_3_higher_is_better_scores_rank_in_descending_order() {
    // REQ: SCR-3, SCR-1
    let keys = [scored(12), scored(11), scored(3), scored(0)];
    assert_eq!(
        places_from_keys(&keys),
        vec![(1, 1), (2, 1), (3, 1), (4, 1)]
    );
    assert_eq!(round_points(&keys), vec![12, 8, 4, 0]);
}

#[test]
fn t_scr_3_higher_is_better_ties_share_place_and_points() {
    // REQ: SCR-3, SCR-8
    let keys = [scored(5), scored(5), scored(0), scored(0)];
    assert_eq!(
        places_from_keys(&keys),
        vec![(1, 2), (1, 2), (3, 2), (3, 2)]
    );
    assert_eq!(round_points(&keys), vec![10, 10, 2, 2]);
}

#[test]
fn t_scr_3_scaling_every_score_by_ten_keeps_points() {
    // REQ: SCR-1
    let plain = [scored(12), scored(11), scored(3), scored(0)];
    let scaled = [scored(120), scored(110), scored(30), scored(0)];
    assert_eq!(round_points(&plain), round_points(&scaled));

    let tied = [scored(5), scored(5), scored(0), scored(0)];
    let tied_scaled = [scored(50), scored(50), scored(0), scored(0)];
    assert_eq!(round_points(&tied), round_points(&tied_scaled));
}

#[test]
fn t_scr_4_no_result_ranks_below_outcomes() {
    // REQ: SCR-5
    let keys = [placed(1), placed(2), no_result(), no_result()];
    assert_eq!(
        places_from_keys(&keys),
        vec![(1, 1), (2, 1), (3, 2), (3, 2)]
    );
    assert_eq!(round_points(&keys), vec![12, 8, 2, 2]);
}

#[test]
fn t_scr_7_early_elimination_ranked_second_gets_eight() {
    // REQ: SCR-5
    // A player eliminated early but ranked 2nd by a 4-player game keeps that
    // outcome's place and therefore 2nd place's points.
    let keys = [placed(1), placed(2), placed(3), placed(4)];
    let points = round_points(&keys);
    assert_eq!(points[1], 8);
    assert_eq!(points, vec![12, 8, 4, 0]);
}

#[test]
fn t_scr_11_every_scored_round_sums_to_six_n() {
    // REQ: SCR-11
    let rounds: [&[(u8, i128)]; 6] = [
        &[placed(1), placed(2)],
        &[placed(1), placed(1)],
        &[placed(1), placed(2), placed(3)],
        &[placed(1), placed(2), placed(2)],
        &[placed(1), placed(2), no_result(), no_result()],
        &[placed(1), no_result(), no_result(), no_result()],
    ];
    for keys in rounds {
        let expected = 6 * keys.len() as u32;
        assert_eq!(
            round_points(keys).iter().sum::<u32>(),
            expected,
            "keys = {keys:?}"
        );
    }
}

proptest! {
    #[test]
    fn t_scr_11_round_points_sum_is_six_n(keys in keys_strategy()) {
        // REQ: SCR-11
        let expected = 6 * keys.len() as u32;
        prop_assert_eq!(round_points(&keys).iter().sum::<u32>(), expected);
    }

    #[test]
    fn t_ver_14_points_for_place_is_average_of_base_points(
        n in 2usize..=4,
        place in 1usize..=4,
        group in 1usize..=4,
    ) {
        // REQ: SCR-6, SCR-7, SCR-8
        prop_assume!(place + group - 1 <= n);
        let total: u32 = (place..place + group).map(|q| base_points(n, q)).sum();
        prop_assert_eq!(total % group as u32, 0, "the average has no remainder");
        prop_assert_eq!(points_for_place(n, place, group), total / group as u32);
    }

    #[test]
    fn t_ver_14_points_for_place_is_at_most_scale(
        n in 2usize..=4,
        place in 1usize..=4,
        group in 1usize..=4,
    ) {
        // REQ: SCR-6
        prop_assume!(place + group - 1 <= n);
        prop_assert!(points_for_place(n, place, group) <= POINTS_SCALE);
    }

    #[test]
    fn t_ver_14_points_for_place_single_group_ends(n in 2usize..=4) {
        // REQ: SCR-7
        prop_assert_eq!(points_for_place(n, 1, 1), 12);
        prop_assert_eq!(points_for_place(n, n, 1), 0);
    }

    #[test]
    fn t_ver_14_points_for_place_single_groups_never_increase(
        n in 2usize..=4,
        place in 1usize..=3,
    ) {
        // REQ: SCR-7
        prop_assume!(place < n);
        prop_assert!(points_for_place(n, place + 1, 1) <= points_for_place(n, place, 1));
    }

    #[test]
    fn t_ver_14_places_from_keys_stay_inside_the_field(keys in keys_strategy()) {
        // REQ: SCR-4
        let n = keys.len();
        let places = places_from_keys(&keys);
        prop_assert_eq!(places.len(), n);
        prop_assert!(places.iter().any(|&(place, _)| place == 1));
        for &(place, group) in &places {
            prop_assert!(place >= 1 && place <= n);
            prop_assert!(group >= 1);
            prop_assert!(place + group - 1 <= n);
        }
    }

    #[test]
    fn t_ver_14_place_and_group_are_key_counts(keys in keys_strategy()) {
        // REQ: SCR-4
        let places = places_from_keys(&keys);
        for (index, &key) in keys.iter().enumerate() {
            let smaller = keys.iter().filter(|&&other| other < key).count();
            let equal = keys.iter().filter(|&&other| other == key).count();
            prop_assert_eq!(places[index], (1 + smaller, equal));
        }
    }

    #[test]
    fn t_ver_14_equal_keys_tie_and_smaller_keys_rank_first(keys in keys_strategy()) {
        // REQ: SCR-4
        let places = places_from_keys(&keys);
        for (i, &left) in keys.iter().enumerate() {
            for (j, &right) in keys.iter().enumerate() {
                if left == right {
                    prop_assert_eq!(places[i], places[j]);
                }
                if left < right {
                    prop_assert!(places[i].0 < places[j].0);
                }
            }
        }
    }

    #[test]
    fn t_ver_14_round_points_follow_key_order(keys in keys_strategy()) {
        // REQ: SCR-4, SCR-6
        let points = round_points(&keys);
        for (i, &left) in keys.iter().enumerate() {
            for (j, &right) in keys.iter().enumerate() {
                if left == right {
                    prop_assert_eq!(points[i], points[j]);
                }
                if left < right {
                    prop_assert!(points[i] >= points[j]);
                }
            }
        }
    }
}
