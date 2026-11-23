//! Scoring adapters: thin wrappers over `superminigames-logic` (SCR-2).
//!
//! The computation itself lives in `logic` (SCR-2); this module only maps each
//! [`PlayerOutcome`] to the SCR-3 sort key.

use crate::outcome::PlayerOutcome;
use superminigames_logic::places::places_from_keys;
use superminigames_logic::points::POINTS_SCALE as LOGIC_POINTS_SCALE;

/// Points are integers at this scale; scoring never uses floating point
/// (SCR-6, CI-3).
pub const POINTS_SCALE: u32 = LOGIC_POINTS_SCALE;

/// The SCR-3 sort key of one outcome.
pub fn key_of(outcome: PlayerOutcome) -> (u8, i128) {
    match outcome {
        PlayerOutcome::Placed(place) => (0, i128::from(place)),
        // Higher scores sort first, so negate before comparing.
        PlayerOutcome::Scored(score) => (0, -i128::from(score)),
        // NoResult ranks below every outcome and ties with the other NoResults
        // (SCR-3, SCR-5).
        PlayerOutcome::NoResult(_) => (1, 0),
    }
}

/// `(place, tie group size)` per outcome, in input order (SCR-2, SCR-3, SCR-4).
pub fn compute_places(outcomes: &[PlayerOutcome]) -> Vec<(usize, usize)> {
    let keys: Vec<(u8, i128)> = outcomes.iter().copied().map(key_of).collect();
    places_from_keys(&keys)
}

/// Points for a tie group of `group` participants starting at `place` (SCR-6).
pub fn points_for_place(n: usize, place: usize, group: usize) -> u32 {
    superminigames_logic::points::points_for_place(n, place, group)
}
