//! T-SCR-8, T-SCR-9, T-PRS-2 and T-PRS-3, plus the FIN-1, PRS-2, PRS-3 and
//! SCR-10 rows of VER-14 for `rank_players`, `next_streak`, `add_points` and
//! `fill_missing_outcome`.
//!
//! VER-19: proptest covers the VER-14 properties until Verus lands in T8.2.

use proptest::prelude::*;
use superminigames_logic::outcome::{MissingOutcome, fill_missing_outcome};
use superminigames_logic::ranking::{add_points, rank_players};
use superminigames_logic::streak::next_streak;

#[test]
fn t_scr_8_final_totals_rank_by_competition_ranking() {
    // REQ: FIN-1
    assert_eq!(rank_players(&[false; 4], &[15, 15, 9, 3]), vec![1, 1, 3, 4]);
}

#[test]
fn t_scr_9_removed_player_ranks_last_whatever_the_totals() {
    // REQ: FIN-1, T-SCR-9
    // A Removed player with 9 points and a remaining player with 3.
    assert_eq!(rank_players(&[true, false], &[9, 3]), vec![2, 1]);
}

#[test]
fn t_scr_9_removed_players_share_a_rank_below_everyone() {
    // REQ: FIN-1
    assert_eq!(
        rank_players(&[false, true, false, true], &[3, 9, 3, 9]),
        vec![1, 3, 1, 3]
    );
}

#[test]
fn t_scr_8_ties_share_a_rank_and_skip_the_next() {
    // REQ: FIN-1
    assert_eq!(rank_players(&[false; 3], &[0, 0, 0]), vec![1, 1, 1]);
    assert_eq!(rank_players(&[false; 3], &[5, 5, 4]), vec![1, 1, 3]);
    assert_eq!(rank_players(&[false; 2], &[4, 5]), vec![2, 1]);
}

#[test]
fn t_prs_2_streak_reaches_k_at_the_third_absent_start() {
    // REQ: PRS-2, PRS-3, T-PRS-2
    let mut streak = 0;
    for _ in 0..2 {
        let (next, removed) = next_streak(streak, false, 3);
        streak = next;
        assert!(!removed, "removal must not happen before the third start");
    }
    assert_eq!(streak, 2);
    assert_eq!(next_streak(streak, false, 3), (3, true));
}

#[test]
fn t_prs_3_present_resets_the_streak() {
    // REQ: PRS-2, T-PRS-3
    // Absent at rounds 5 and 6, present at round 7, absent again at 8 and 9.
    let (streak, removed) = next_streak(2, true, 3);
    assert_eq!((streak, removed), (0, false));
    let (streak, removed) = next_streak(streak, false, 3);
    assert_eq!((streak, removed), (1, false));
    assert_eq!(next_streak(streak, false, 3), (2, false));
}

#[test]
fn t_prs_3_removal_happens_on_the_start_that_reaches_k() {
    // REQ: PRS-3
    assert_eq!(next_streak(0, false, 1), (1, true));
    assert_eq!(next_streak(2, false, 3), (3, true));
    assert_eq!(next_streak(9, false, 10), (10, true));
}

#[test]
fn t_scr_10_points_are_added_only_for_scored_rounds() {
    // REQ: SCR-10
    assert_eq!(add_points(0, 0), 0);
    assert_eq!(add_points(6, 6), 12);
    assert_eq!(add_points(12 * 99, 12), 12 * 100);
}

#[test]
fn t_rnd_9_missing_outcome_follows_left_and_result() {
    // REQ: RND-9
    assert_eq!(fill_missing_outcome(true, false), MissingOutcome::Keep);
    assert_eq!(fill_missing_outcome(true, true), MissingOutcome::Keep);
    assert_eq!(fill_missing_outcome(false, true), MissingOutcome::LeftRound);
    assert_eq!(
        fill_missing_outcome(false, false),
        MissingOutcome::TimerExpired
    );
}

proptest! {
    #[test]
    fn t_ver_14_ranks_are_one_plus_the_players_that_sort_before(
        removed in prop::collection::vec(any::<bool>(), 1..=4),
        totals in prop::collection::vec(0u32..=12_000, 1..=4),
    ) {
        // REQ: FIN-1
        prop_assume!(removed.len() == totals.len());
        let ranks = rank_players(&removed, &totals);
        prop_assert_eq!(ranks.len(), totals.len());
        for (i, rank) in ranks.iter().enumerate() {
            let before = (0..totals.len())
                .filter(|&j| {
                    let order = (removed[j], std::cmp::Reverse(totals[j]));
                    let own = (removed[i], std::cmp::Reverse(totals[i]));
                    order < own
                })
                .count();
            prop_assert_eq!(*rank, 1 + before as u32);
        }
    }

    #[test]
    fn t_ver_14_removed_players_never_rank_better(
        removed in prop::collection::vec(any::<bool>(), 1..=4),
        totals in prop::collection::vec(0u32..=12_000, 1..=4),
    ) {
        // REQ: FIN-1
        prop_assume!(removed.len() == totals.len());
        let ranks = rank_players(&removed, &totals);
        for i in 0..totals.len() {
            for j in 0..totals.len() {
                if removed[i] && !removed[j] {
                    prop_assert!(ranks[i] > ranks[j]);
                }
                if removed[i] == removed[j] && totals[i] == totals[j] {
                    prop_assert_eq!(ranks[i], ranks[j]);
                }
                if removed[i] == removed[j] && totals[i] > totals[j] {
                    prop_assert!(ranks[i] <= ranks[j]);
                }
            }
        }
    }

    #[test]
    fn t_ver_14_streak_rules(streak in 0u32..=1_000, present in any::<bool>(), k in 1u32..=10) {
        // REQ: PRS-2, PRS-3
        let (next, removed) = next_streak(streak, present, k);
        if present {
            prop_assert_eq!(next, 0);
        } else {
            prop_assert_eq!(next, streak + 1);
        }
        prop_assert_eq!(removed, next >= k);
    }

    #[test]
    fn t_ver_14_add_points_never_overflows(total in 0u32..=(12 * 99), points in 0u32..=12) {
        // REQ: SCR-10
        prop_assert_eq!(add_points(total, points), total + points);
    }

    #[test]
    fn t_ver_14_fill_missing_outcome_keeps_a_present_result(
        has_result in any::<bool>(),
        left in any::<bool>(),
    ) {
        // REQ: RND-9
        let filled = fill_missing_outcome(has_result, left);
        if has_result {
            prop_assert_eq!(filled, MissingOutcome::Keep);
        } else if left {
            prop_assert_eq!(filled, MissingOutcome::LeftRound);
        } else {
            prop_assert_eq!(filled, MissingOutcome::TimerExpired);
        }
    }
}
