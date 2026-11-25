//! Points per place and per round (SCR-6, SCR-7, SCR-11).

use crate::places::places_from_keys;

/// Points are integers at scale 12; scoring never uses floating point
/// (SCR-6, SCR-10, CI-3).
pub const POINTS_SCALE: u32 = 12;

/// Points for a tie group of `group` participants starting at `place` (SCR-6).
///
/// A tied group receives the average of the base points of the places it
/// occupies; at scale 12 that average has no remainder for `n` in 2..=4.
pub fn points_for_place(n: usize, place: usize, group: usize) -> u32 {
    debug_assert!((2..=4).contains(&n));
    debug_assert!(place >= 1 && group >= 1 && place + group - 1 <= n);
    let (n, p, g) = (n as u32, place as u32, group as u32);
    6 * (2 * n + 1 - 2 * p - g) / (n - 1)
}

/// Points for every key, in input order (SCR-4, SCR-6, SCR-11).
pub fn round_points(keys: &[(u8, i128)]) -> Vec<u32> {
    let n = keys.len();
    debug_assert!(
        n >= 2,
        "SCR-6: a round scores at least two participants (SCP-4)"
    );
    places_from_keys(keys)
        .into_iter()
        .map(|(place, group)| points_for_place(n, place, group))
        .collect()
}
