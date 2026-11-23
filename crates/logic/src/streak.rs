//! Absence streak at a round start (PRS-2, PRS-3).

/// New `(absent_streak, removed)` for one InMatch player at a round start.
///
/// PRS-2: Present resets the streak to 0. PRS-3: absent adds 1 and the player is
/// removed exactly when the new streak reaches `k`. The addition saturates, so a
/// streak can never wrap (VER-14). Only round-start checks call this (PRS-8).
pub fn next_streak(streak: u32, present: bool, k: u32) -> (u32, bool) {
    if present {
        (0, false)
    } else {
        let next = streak.saturating_add(1);
        (next, next >= k)
    }
}
