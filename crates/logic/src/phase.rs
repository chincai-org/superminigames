//! Engine phase machine (section 12.1, ARC-5).
//!
//! The table of section 12.1 lists a `Guard` column that this pure transition
//! cannot evaluate (it has no lobby state), so a guard is the caller's job: the
//! trigger is only produced once the guard holds. The `(none) -> Lobby` row is
//! the actor's initial value, not a transition, so it has no trigger here.

/// Engine phase (section 12.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Phase {
    /// The actor exists and the match has not started.
    Lobby,
    /// A round slot is starting: presence check, participants, pool, attempts.
    Starting,
    /// A game is running.
    InRound,
    /// A slot has been processed; totals are updated.
    RoundFinished,
    /// Terminal: the match is over.
    MatchFinished,
    /// Terminal: the match was abandoned.
    Cancelled,
}

/// One variant per trigger row of the table in section 12.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PhaseTrigger {
    /// `StartMatch` from the host; the caller has checked every guard.
    StartMatch,
    /// `Cancel` from the host.
    Cancel,
    /// The host left or disconnected.
    HostLeft,
    /// The round-start presence check found `InMatch <= 1` (RND-2).
    PresenceCheckFailed,
    /// The slot was skipped: NotEnoughPlayers, EmptyPool or StartFailed (RND-3).
    SlotSkipped,
    /// A game instance initialized successfully (RND-4).
    AttemptSucceeded,
    /// The game finished, the deadline was reached, or every participant left.
    RoundEnded,
    /// A fatal error, a panic, or an invalid result: the slot is skipped (RND-9).
    RoundAborted,
    /// `RoundFinished` with slots left (MTH-1).
    MoreSlotsRemain,
    /// `RoundFinished` after the last slot (MTH-1).
    LastSlotProcessed,
}

/// The next phase, or `None` when the trigger does not apply to `phase`
/// (section 12.1). `Cancelled` and `MatchFinished` are terminal, so every
/// trigger returns `None` from them (VER-14).
pub fn next_phase(phase: Phase, trigger: PhaseTrigger) -> Option<Phase> {
    match (phase, trigger) {
        (Phase::Lobby, PhaseTrigger::StartMatch) => Some(Phase::Starting),
        (Phase::Lobby, PhaseTrigger::Cancel | PhaseTrigger::HostLeft) => Some(Phase::Cancelled),
        (Phase::Starting, PhaseTrigger::PresenceCheckFailed) => Some(Phase::MatchFinished),
        (Phase::Starting, PhaseTrigger::SlotSkipped | PhaseTrigger::RoundAborted) => {
            Some(Phase::RoundFinished)
        }
        (Phase::Starting, PhaseTrigger::AttemptSucceeded) => Some(Phase::InRound),
        (Phase::InRound, PhaseTrigger::RoundEnded | PhaseTrigger::RoundAborted) => {
            Some(Phase::RoundFinished)
        }
        (Phase::RoundFinished, PhaseTrigger::MoreSlotsRemain) => Some(Phase::Starting),
        (Phase::RoundFinished, PhaseTrigger::LastSlotProcessed) => Some(Phase::MatchFinished),
        (Phase::MatchFinished | Phase::Cancelled, _) => None,
        _ => None,
    }
}
