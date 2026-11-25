//! T1.3: every transition of the engine phase table (section 12.1) and the
//! transitions that MUST NOT happen (VER-14: `Cancelled` and `MatchFinished`
//! are absorbing, `Cancelled` is reachable only from `Lobby`, `InRound` is
//! reachable only from `Starting`).

use std::collections::BTreeSet;

use superminigames_logic::phase::{Phase, PhaseTrigger, next_phase};

const PHASES: [Phase; 6] = [
    Phase::Lobby,
    Phase::Starting,
    Phase::InRound,
    Phase::RoundFinished,
    Phase::MatchFinished,
    Phase::Cancelled,
];

const TRIGGERS: [PhaseTrigger; 10] = [
    PhaseTrigger::StartMatch,
    PhaseTrigger::Cancel,
    PhaseTrigger::HostLeft,
    PhaseTrigger::PresenceCheckFailed,
    PhaseTrigger::SlotSkipped,
    PhaseTrigger::AttemptSucceeded,
    PhaseTrigger::RoundEnded,
    PhaseTrigger::RoundAborted,
    PhaseTrigger::MoreSlotsRemain,
    PhaseTrigger::LastSlotProcessed,
];

/// One row of the table in section 12.1.
fn transition(phase: Phase, trigger: PhaseTrigger) -> Option<Phase> {
    next_phase(phase, trigger)
}

#[test]
fn t_arc_5_every_row_of_the_section_12_1_table() {
    // REQ: ARC-5
    assert_eq!(
        transition(Phase::Lobby, PhaseTrigger::StartMatch),
        Some(Phase::Starting)
    );
    assert_eq!(
        transition(Phase::Lobby, PhaseTrigger::Cancel),
        Some(Phase::Cancelled)
    );
    assert_eq!(
        transition(Phase::Lobby, PhaseTrigger::HostLeft),
        Some(Phase::Cancelled)
    );
    assert_eq!(
        transition(Phase::Starting, PhaseTrigger::PresenceCheckFailed),
        Some(Phase::MatchFinished)
    );
    assert_eq!(
        transition(Phase::Starting, PhaseTrigger::SlotSkipped),
        Some(Phase::RoundFinished)
    );
    assert_eq!(
        transition(Phase::Starting, PhaseTrigger::AttemptSucceeded),
        Some(Phase::InRound)
    );
    assert_eq!(
        transition(Phase::InRound, PhaseTrigger::RoundEnded),
        Some(Phase::RoundFinished)
    );
    assert_eq!(
        transition(Phase::InRound, PhaseTrigger::RoundAborted),
        Some(Phase::RoundFinished)
    );
    assert_eq!(
        transition(Phase::RoundFinished, PhaseTrigger::MoreSlotsRemain),
        Some(Phase::Starting)
    );
    assert_eq!(
        transition(Phase::RoundFinished, PhaseTrigger::LastSlotProcessed),
        Some(Phase::MatchFinished)
    );
}

#[test]
fn t_arc_5_finished_and_cancelled_are_absorbing() {
    // REQ: ARC-5, 12.1
    for terminal in [Phase::MatchFinished, Phase::Cancelled] {
        for trigger in TRIGGERS {
            assert_eq!(
                transition(terminal, trigger),
                None,
                "{terminal:?} must be terminal, got a transition for {trigger:?}"
            );
        }
    }
}

#[test]
fn t_arc_5_cancelled_is_reachable_only_from_lobby() {
    // REQ: ARC-5, 12.1
    for phase in PHASES {
        for trigger in TRIGGERS {
            if transition(phase, trigger) == Some(Phase::Cancelled) {
                assert_eq!(phase, Phase::Lobby, "guard for {trigger:?}");
            }
        }
    }
}

#[test]
fn t_arc_5_in_round_is_reachable_only_from_starting() {
    // REQ: ARC-5, 12.1
    for phase in PHASES {
        for trigger in TRIGGERS {
            if transition(phase, trigger) == Some(Phase::InRound) {
                assert_eq!(phase, Phase::Starting, "guard for {trigger:?}");
            }
        }
    }
}

#[test]
fn t_arc_5_every_returned_transition_is_in_the_table() {
    // REQ: ARC-5, 12.1
    // The whole reachable table, listed independently of the implementation.
    let table: BTreeSet<(Phase, PhaseTrigger, Phase)> = [
        (Phase::Lobby, PhaseTrigger::StartMatch, Phase::Starting),
        (Phase::Lobby, PhaseTrigger::Cancel, Phase::Cancelled),
        (Phase::Lobby, PhaseTrigger::HostLeft, Phase::Cancelled),
        (
            Phase::Starting,
            PhaseTrigger::PresenceCheckFailed,
            Phase::MatchFinished,
        ),
        (
            Phase::Starting,
            PhaseTrigger::SlotSkipped,
            Phase::RoundFinished,
        ),
        (
            Phase::Starting,
            PhaseTrigger::RoundAborted,
            Phase::RoundFinished,
        ),
        (
            Phase::Starting,
            PhaseTrigger::AttemptSucceeded,
            Phase::InRound,
        ),
        (
            Phase::InRound,
            PhaseTrigger::RoundEnded,
            Phase::RoundFinished,
        ),
        (
            Phase::InRound,
            PhaseTrigger::RoundAborted,
            Phase::RoundFinished,
        ),
        (
            Phase::RoundFinished,
            PhaseTrigger::MoreSlotsRemain,
            Phase::Starting,
        ),
        (
            Phase::RoundFinished,
            PhaseTrigger::LastSlotProcessed,
            Phase::MatchFinished,
        ),
    ]
    .into_iter()
    .collect();

    for phase in PHASES {
        for trigger in TRIGGERS {
            let to = transition(phase, trigger);
            assert_eq!(
                to.is_some_and(|to| table.contains(&(phase, trigger, to))),
                to.is_some(),
                "{phase:?} + {trigger:?} -> {to:?} is not a row of section 12.1"
            );
        }
    }
}

#[test]
fn t_arc_5_lobby_is_left_only_by_start_cancel_or_host_leaving() {
    // REQ: ARC-5, 12.1
    for trigger in TRIGGERS {
        let to = transition(Phase::Lobby, trigger);
        let expected = match trigger {
            PhaseTrigger::StartMatch => Some(Phase::Starting),
            PhaseTrigger::Cancel | PhaseTrigger::HostLeft => Some(Phase::Cancelled),
            _ => None,
        };
        assert_eq!(to, expected, "Lobby + {trigger:?}");
    }
}

#[test]
fn t_arc_5_round_finished_moves_on_or_ends_the_match() {
    // REQ: ARC-5, 12.1
    for trigger in TRIGGERS {
        let to = transition(Phase::RoundFinished, trigger);
        let expected = match trigger {
            PhaseTrigger::MoreSlotsRemain => Some(Phase::Starting),
            PhaseTrigger::LastSlotProcessed => Some(Phase::MatchFinished),
            _ => None,
        };
        assert_eq!(to, expected, "RoundFinished + {trigger:?}");
    }
}
