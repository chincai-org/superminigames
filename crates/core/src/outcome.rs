//! Per-player round outcomes (TYP-1, section 9).

use serde::{Deserialize, Serialize};

use crate::ids::PlayerId;

/// Why the engine recorded no outcome for a participant (TYP-1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DisqualificationReason {
    /// The round reached its hard cap (TMR-4).
    TimerExpired,
    /// The participant left the round and the game reported nothing (PRS-6).
    LeftRound,
}

/// One participant's outcome for a round (TYP-1, SCR-3).
///
/// `Placed` and `Scored` are normal outcomes, including in-game elimination
/// (PRS-11). `NoResult` ranks below every outcome (SCR-5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlayerOutcome {
    /// Final place from the game; ties repeat the value.
    Placed(u8),
    /// Raw score; `HigherIsBetter` games only.
    Scored(i64),
    /// Nothing determined for this participant.
    NoResult(DisqualificationReason),
}

/// The participant's outcome in `outcomes`, if the game reported one (RND-9).
pub fn outcome_of(
    outcomes: &[(PlayerId, PlayerOutcome)],
    player: PlayerId,
) -> Option<PlayerOutcome> {
    outcomes
        .iter()
        .find(|(reported, _)| *reported == player)
        .map(|(_, outcome)| *outcome)
}
