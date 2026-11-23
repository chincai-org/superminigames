//! Round-result fill-in rule (RND-9).

/// How the engine fills a participant outcome the game did not report (RND-9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingOutcome {
    /// The game reported an outcome for this participant; it stands (PRS-11).
    Keep,
    /// The participant left the round and the game reported nothing (PRS-6).
    LeftRound,
    /// The round reached its hard cap without an outcome (TMR-4).
    TimerExpired,
}

/// RND-9 fill-in: a reported outcome is kept, a missing one becomes
/// `LeftRound` when the player left the round and `TimerExpired` otherwise
/// (PRS-6, TMR-4). The timer loop calls this instead of inlining the rule
/// (VER-16).
pub fn fill_missing_outcome(has_result: bool, left: bool) -> MissingOutcome {
    match (has_result, left) {
        (true, _) => MissingOutcome::Keep,
        (false, true) => MissingOutcome::LeftRound,
        (false, false) => MissingOutcome::TimerExpired,
    }
}
