//! Final match ranking (FIN-1).

use crate::places::places_from_keys;

/// Final ranks for every seated player, in seat order (FIN-1).
///
/// The order is "Removed last, then `total_points` descending", keyed exactly
/// like SCR-3, so competition ranking comes from [`places_from_keys`]:
/// a Removed player ranks below every player who was never Removed whatever the
/// totals, and players share a rank only inside the same group with equal
/// totals (FIN-1 has no tiebreaker). Ties keep seat order because the output is
/// in input order.
pub fn rank_players(removed: &[bool], totals: &[u32]) -> Vec<u32> {
    debug_assert_eq!(removed.len(), totals.len());
    let keys: Vec<(u8, i128)> = removed
        .iter()
        .zip(totals)
        .map(|(&is_removed, &total)| (u8::from(is_removed), -i128::from(total)))
        .collect();
    places_from_keys(&keys)
        .into_iter()
        .map(|(place, _group)| place as u32)
        .collect()
}

/// Adds a round's points to a running total (SCR-10).
///
/// Totals are `u32` and points are added only for `Scored` rounds. A round
/// awards at most `POINTS_SCALE` points (SCR-6, VER-14) and a match has at most
/// 100 rounds, so the total stays below `12 * 100` and cannot overflow; the
/// debug assertions state that precondition rather than hiding a wrap.
pub fn add_points(total: u32, points: u32) -> u32 {
    debug_assert!(points <= crate::points::POINTS_SCALE);
    debug_assert!(total <= crate::points::POINTS_SCALE * 100);
    total + points
}
