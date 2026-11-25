//! Replay recording and replaying (REP-1, REP-2, section 13).
//!
//! A [`ReplayRecorder`] captures the header of a round and every `(at_ms, input)`
//! in the exact order it was ticked; [`replay`] runs a [`ReplayLog`] on a fresh
//! instance. Both are synchronous and runtime-free, so the actor owns the clock
//! and this module only sees the values it is given (GAM-1, TMR-6).
//!
//! A panicking game is a failure, never a crash of the caller: [`replay`] wraps
//! every callback and maps a panic to [`GameError::Fatal`] (FLR-7).

use std::panic::{AssertUnwindSafe, catch_unwind};

use superminigames_core::game::{GameError, GameEvent, GameFactory, GameInput, GameResult};
use superminigames_core::ids::{GameId, PlayerId};

use crate::report::ReplayLog;

/// Collects a [`ReplayLog`] while a round runs (REP-1).
///
/// It is created once a round reaches play and is only fed ticked inputs, so
/// the recorded order equals the tick order (TMR-6).
pub struct ReplayRecorder {
    log: ReplayLog,
}

impl ReplayRecorder {
    /// Starts recording a round: the header is fixed now, inputs are appended
    /// by [`record`](Self::record) (REP-1).
    pub fn new(game: GameId, game_seed: u64, players: Vec<PlayerId>, time_limit_ms: u64) -> Self {
        Self {
            log: ReplayLog {
                game,
                game_seed,
                players,
                time_limit_ms,
                inputs: Vec::new(),
            },
        }
    }

    /// Appends one ticked input (REP-1).
    pub fn record(&mut self, at_ms: u64, input: GameInput) {
        self.log.inputs.push((at_ms, input));
    }

    /// The finished log (REP-1).
    pub fn into_log(self) -> ReplayLog {
        self.log
    }
}

/// What one replay produced (REP-2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayOutcome {
    /// Every event, in tick order.
    pub events: Vec<GameEvent>,
    /// The final result, collected after the last input (REP-2).
    pub result: GameResult,
}

/// Replays `log` on a fresh instance (REP-2): `initialize` with the recorded
/// header, then each `(at_ms, input)` in order. Returns the events and the
/// final `result()` so T-REP-1 can compare them with the original round.
pub fn replay(factory: GameFactory, log: &ReplayLog) -> Result<ReplayOutcome, GameError> {
    let mut game = guard(factory, "create")?;
    guard(
        || game.initialize(log.game_seed, &log.players, log.time_limit_ms),
        "initialize",
    )??;
    let mut events = Vec::new();
    for (at_ms, input) in &log.inputs {
        events.extend(guard(|| game.tick(*at_ms, input.clone()), "tick")??);
    }
    let result = guard(|| game.result(), "result")?;
    Ok(ReplayOutcome { events, result })
}

/// Runs one game callback, mapping a panic to `GameError::Fatal` so a buggy
/// game cannot crash the caller (FLR-7). REP-2 itself does not require this, but
/// the engine's panic rule applies to every game callback (T2.5).
fn guard<T>(call: impl FnOnce() -> T, what: &str) -> Result<T, GameError> {
    match catch_unwind(AssertUnwindSafe(call)) {
        Ok(value) => Ok(value),
        Err(_) => Err(GameError::Fatal(format!("game panicked during {what}"))),
    }
}
