//! The round runner: start attempts, the in-round loop, result validation and
//! the `RoundRecord` (RND-5, RND-6, RND-8..RND-13, FLR-1..FLR-7,
//! SCR-2..SCR-11, TMR-1..TMR-11, PRS-5..PRS-11, REP-1).
//!
//! Everything here is synchronous and runtime-free: time enters only as the
//! `tokio::time::Instant` values the caller injects, and the actor (T2.6) owns
//! the channel waits and the timers. Every `Game` call is wrapped in
//! `catch_unwind` (FLR-7); a panic is a failure, never a crash of the actor.

use std::panic::{AssertUnwindSafe, catch_unwind};

use std::time::Duration;

use superminigames_core::game::{Game, GameAction, GameError, GameEvent, GameInput, ScoringMode};
use superminigames_core::ids::{GameId, PlayerId};
use superminigames_core::outcome::{DisqualificationReason, PlayerOutcome, outcome_of};
use superminigames_core::scoring::{compute_places, points_for_place};
use superminigames_logic::outcome::{MissingOutcome, fill_missing_outcome};
use tokio::time::Instant;

use crate::presence::ReconnectGrace;
use crate::replay::ReplayRecorder;
use crate::report::{PlayerState, PlayerStatus, ReplayLog, RoundRecord, RoundStatus, SkipReason};
use crate::selection::{OwnedPick, RoundPick};

/// A slot gets one attempt plus three retries (FLR-1).
pub const START_ATTEMPTS: u8 = 4;

/// Per-input lag credit cap (TMR-11, SEC-9).
pub const LAG_CAP_MS: u32 = 1_000;

/// Total lag credit one player may be given in a round (TMR-11, SEC-9).
pub const LAG_TOTAL_CAP_MS: u32 = 10_000;

/// Why a slot did not reach play (RND-5, RND-6, FLR-3, FLR-4).
///
/// Either every attempt to `initialize` failed, or the first `initialize`
/// returned a `Fatal` error, which is never retried. Either way the slot
/// consumes its round index (FLR-3, FLR-4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StartFailed {
    /// The number of attempts made: [`START_ATTEMPTS`] when retries were
    /// exhausted, or the attempt that returned `Fatal` (FLR-6).
    pub attempts: u8,
    /// `initialize` returned `GameError::Fatal`, so there was no retry and the
    /// slot is `Skipped(GameAborted)` (FLR-4, RND-5).
    pub fatal: bool,
}

impl StartFailed {
    /// The round status of a slot that did not reach play (FLR-3, FLR-4).
    pub fn status(self) -> RoundStatus {
        if self.fatal {
            RoundStatus::Skipped(SkipReason::GameAborted)
        } else {
            RoundStatus::Skipped(SkipReason::StartFailed)
        }
    }
}

/// One successful tick (TMR-3, GAM-6, GAM-8).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ticked {
    /// The events the game returned for this input (GAM-6).
    pub events: Vec<GameEvent>,
    /// `is_finished()` after the tick, so the round ends here (TMR-3).
    pub finished: bool,
}

/// Why a participant's input was not fed to the game (TMR-4, PRS-5, PRS-7).
///
/// These are the engine's own gates; a game-level rejection is an
/// `InputRejected` event instead (GAM-5). A game abort is not an error here: it
/// is reported by [`RoundStep::aborted`], so the actor ends the round (FLR-4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RoundInputError {
    /// The round is over: it finished, aborted, or the deadline has passed
    /// (TMR-4).
    #[error("the round is over")]
    NotInRound,
    /// The player is not a participant in this round (PRS-5, RND-3).
    #[error("the player is not a participant in this round")]
    NotParticipant,
    /// The player's reconnect grace expired, so they are out for this round
    /// (PRS-7).
    #[error("the player left this round")]
    PlayerLeftRound,
}

/// Something the runner reports while a round runs (GAM-6, PRS-6).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoundEvent {
    /// An event the game produced for a tick (GAM-6).
    Game(GameEvent),
    /// A participant's presence changed mid-round (PRS-6, PRS-11).
    PlayerStatus {
        player: PlayerId,
        status: PlayerStatus,
    },
}

/// The result of one in-round step (TMR-2, TMR-3, FLR-4).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoundStep {
    /// Everything reported for this step, game events first, in order (GAM-6).
    pub events: Vec<RoundEvent>,
    /// `is_finished()` after this step, so the round ends here (TMR-3).
    pub finished: bool,
    /// The game aborted (a `Fatal` error or a panic), so the round ends with no
    /// retry (FLR-4, FLR-7).
    pub aborted: bool,
}

/// A live round: one initialized instance, its attempt count and, when enabled,
/// its replay recorder (RND-6, RND-12, REP-1).
pub struct RoundRunner {
    game: Box<dyn Game>,
    game_id: GameId,
    /// Participants in seat order (RND-11); records and tie order use this.
    participants: Vec<PlayerId>,
    attempts: u8,
    /// Cached from the registered game before play; never read off the live game
    /// (SCR-1, RND-9).
    scoring_mode: ScoringMode,
    recorder: Option<ReplayRecorder>,
    /// The last tick's `at_ms`; ticks never move backwards (TMR-1).
    last_at_ms: u64,
    /// When `InRound` began; `at_ms` is measured from here (TMR-1).
    round_start: Instant,
    /// `round_start + time_limit_ms`, a hard cap for every game (TMR-1, TMR-4).
    deadline: Instant,
    /// Reconnect deadlines for participants who disconnected mid-round (PRS-6).
    grace: ReconnectGrace,
    /// Participants currently away, so a flap reports one `Away` (PRS-6).
    away: Vec<PlayerId>,
    /// Participants whose grace expired; out for this round only (PRS-7).
    left: Vec<PlayerId>,
    /// `(player, total credit given)` for this round (TMR-11).
    lag_credit: Vec<(PlayerId, u32)>,
    /// A game callback failed; the round ends with no retry (FLR-4, FLR-7).
    aborted: bool,
    /// The game reported `is_finished()` (TMR-3).
    finished: bool,
}

impl RoundRunner {
    /// Runs the start attempts for one slot, capturing `round_start` from
    /// `clock` only on the successful `initialize` (RND-5, RND-6,
    /// FLR-1..FLR-4, RNG-6, RND-11, RND-12, TMR-1).
    ///
    /// Each attempt calls `RegisteredGame::create()` for a fresh instance and
    /// `initialize(game_seed, players, time_limit_ms)` with the same seed; a
    /// retry never touches the RNG (RNG-6). `participants` is in seat order and
    /// `first_mover_seat` (RNG-11) reorders them so `players[0]` moves first.
    /// An `InitFailed` error or a panic is retried (FLR-1, FLR-2); a `Fatal`
    /// error is not, it ends the slot at once (FLR-4).
    ///
    /// TMR-1, FLR-2: `clock` is called exactly once, on the attempt that
    /// succeeds, so the deadline is captured when the phase becomes `InRound`
    /// and a retry cannot shorten play. Anyone already disconnected is seeded
    /// by [`seed_disconnected`](Self::seed_disconnected) right after this call,
    /// so the caller keeps the `Away` events (RND-13).
    pub fn start_with_clock(
        pick: &RoundPick<'_>,
        participants: &[PlayerId],
        first_mover_seat: usize,
        time_limit_ms: u64,
        record_replays: bool,
        clock: impl FnMut() -> Instant,
    ) -> Result<Box<Self>, StartFailed> {
        Self::start_owned_with_clock(
            pick.game.owned_pick(pick.game_seed),
            participants,
            first_mover_seat,
            time_limit_ms,
            record_replays,
            clock,
        )
    }

    /// [`start_with_clock`](Self::start_with_clock) from an [`OwnedPick`], for a
    /// caller that runs the slot start on a task (T2.6, T-PRS-9).
    ///
    /// The pick owns the game id, factory and scoring mode cached at
    /// registration (TYP-2), so nothing borrows the registry (RND-12). Every
    /// game callback is still wrapped in `catch_unwind`, so a panicking factory
    /// is a failed attempt (FLR-1, FLR-7).
    pub fn start_owned_with_clock(
        pick: OwnedPick,
        participants: &[PlayerId],
        first_mover_seat: usize,
        time_limit_ms: u64,
        record_replays: bool,
        mut clock: impl FnMut() -> Instant,
    ) -> Result<Box<Self>, StartFailed> {
        let players = first_mover_order(participants, first_mover_seat);
        let mut attempts: u8 = 0;
        while attempts < START_ATTEMPTS {
            attempts += 1;
            // RND-12: a fresh instance every attempt, on a retry too.
            let created = catch_unwind(AssertUnwindSafe(|| (pick.factory)()));
            let Ok(mut game) = created else {
                continue; // a panicking factory is a failed attempt (FLR-1, FLR-7)
            };
            let initialized = catch_unwind(AssertUnwindSafe(|| {
                game.initialize(pick.game_seed, &players, time_limit_ms)
            }));
            match initialized {
                Ok(Ok(())) => {
                    // TMR-1, FLR-2: capture the clock only now, on the slot that
                    // reaches play, so a retry never shortens the deadline.
                    let round_start = clock();
                    // REP-1: the header is fixed at start; inputs are appended later.
                    let recorder = if record_replays {
                        Some(ReplayRecorder::new(
                            pick.game_id.clone(),
                            pick.game_seed,
                            players,
                            time_limit_ms,
                        ))
                    } else {
                        None
                    };
                    let runner = Box::new(Self {
                        game,
                        game_id: pick.game_id,
                        participants: participants.to_vec(),
                        attempts,
                        scoring_mode: pick.scoring_mode,
                        recorder,
                        last_at_ms: 0,
                        round_start,
                        deadline: round_start + Duration::from_millis(time_limit_ms),
                        grace: ReconnectGrace::new(),
                        away: Vec::new(),
                        left: Vec::new(),
                        lag_credit: Vec::new(),
                        aborted: false,
                        finished: false,
                    });
                    // RND-13: the caller seeds anyone already disconnected with
                    // `seed_disconnected`, so nothing is dropped here.
                    return Ok(runner);
                }
                // RND-5, FLR-4: `Fatal` is terminal for the slot, with no retry.
                Ok(Err(GameError::Fatal(_))) => {
                    return Err(StartFailed {
                        attempts,
                        fatal: true,
                    });
                }
                // `InitFailed` or a panic: retry (FLR-1, FLR-2).
                Ok(Err(GameError::InitFailed(_))) | Err(_) => {}
            }
        }
        Err(StartFailed {
            attempts,
            fatal: false,
        })
    }

    /// [`start_with_clock`](Self::start_with_clock) at an injected `round_start`,
    /// for tests and an actor that already read the clock (TMR-1).
    pub fn start_at(
        pick: &RoundPick<'_>,
        participants: &[PlayerId],
        first_mover_seat: usize,
        time_limit_ms: u64,
        record_replays: bool,
        round_start: Instant,
    ) -> Result<Box<Self>, StartFailed> {
        Self::start_with_clock(
            pick,
            participants,
            first_mover_seat,
            time_limit_ms,
            record_replays,
            || round_start,
        )
    }

    /// [`start_with_clock`](Self::start_with_clock) at
    /// `tokio::time::Instant::now()`, captured after a successful `initialize`
    /// (TMR-1).
    pub fn start(
        pick: &RoundPick<'_>,
        participants: &[PlayerId],
        first_mover_seat: usize,
        time_limit_ms: u64,
        record_replays: bool,
    ) -> Result<Box<Self>, StartFailed> {
        Self::start_with_clock(
            pick,
            participants,
            first_mover_seat,
            time_limit_ms,
            record_replays,
            Instant::now,
        )
    }

    /// Seeds reconnect grace at `round_start` for participants whose presence
    /// flag was false right after `initialize` (RND-13, PRS-6, T-PRS-9).
    ///
    /// Each unique participant that is still in this round reports `Away` once
    /// and gets a grace deadline of exactly `round_start + RECONNECT_GRACE`; a
    /// repeated id, a non-participant or an already-left player is ignored. The
    /// returned events are for the caller to forward (PRS-6).
    pub fn seed_disconnected(&mut self, disconnected: &[PlayerId]) -> Vec<RoundEvent> {
        let round_start = self.round_start;
        let mut events = Vec::new();
        let mut seen: Vec<PlayerId> = Vec::new();
        for &player in disconnected {
            if seen.contains(&player) {
                continue;
            }
            seen.push(player);
            if !self.participants.contains(&player) || self.left.contains(&player) {
                continue;
            }
            events.extend(self.on_connection(player, false, round_start));
        }
        events
    }

    /// The round's hard deadline (TMR-1, TMR-4).
    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    /// When the phase became `InRound`; the actor adds a `next_wakeup_ms` value
    /// to this to arm the wake-up timer (TMR-1, TMR-2).
    pub fn round_start(&self) -> Instant {
        self.round_start
    }

    /// The earliest reconnect-grace expiry, for the actor's TMR-2 timer
    /// (PRS-6).
    pub fn next_grace_deadline(&self) -> Option<Instant> {
        self.grace.next_deadline()
    }
    /// Participants in seat order (RND-11).
    pub fn participants(&self) -> &[PlayerId] {
        &self.participants
    }

    /// The selected game's id, for the `RoundStarting` event (ARC-3).
    pub fn game_id(&self) -> &GameId {
        &self.game_id
    }

    /// The participants whose grace expired this round, in expiry order
    /// (PRS-7). [`finish`](Self::finish) always folds this set into the
    /// `NoResult(LeftRound)` fill, whatever `left` the caller passes.
    pub fn left(&self) -> &[PlayerId] {
        &self.left
    }

    /// Whether the game finished or aborted, or every participant is out, so the
    /// round is over now (RND-13, TMR-3, FLR-4).
    pub fn ended(&self) -> bool {
        self.aborted || self.finished || self.all_left()
    }

    /// Whether a game callback failed and the round ends with
    /// `Skipped(GameAborted)` (FLR-4, FLR-7).
    pub fn aborted(&self) -> bool {
        self.aborted
    }

    /// Whether the game reported `is_finished()` (TMR-3).
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Whether every participant's grace has expired (RND-13, PRS-10).
    pub fn all_left(&self) -> bool {
        !self.participants.is_empty() && self.left.len() >= self.participants.len()
    }

    /// The next moment the game needs a tick, in ms since `round_start`
    /// (GAM-6); `None` when it needs none. A panicking callback aborts the
    /// round, as every other callback does (FLR-7).
    pub fn next_wakeup_ms(&mut self) -> Option<u64> {
        match catch_unwind(AssertUnwindSafe(|| self.game.next_wakeup_ms())) {
            Ok(wakeup) => wakeup,
            Err(_) => {
                self.aborted = true;
                None
            }
        }
    }

    /// The `at_ms` for an event received at `received_at` (TMR-1): whole
    /// milliseconds since `round_start`, never before `round_start` and never
    /// below the previous tick.
    fn at_ms(&self, received_at: Instant) -> u64 {
        // TMR-1: whole milliseconds since `round_start`. Saturate instead of
        // truncating, so a long round cannot wrap `at_ms` back to zero.
        let elapsed = u64::try_from(
            received_at
                .saturating_duration_since(self.round_start)
                .as_millis(),
        )
        .unwrap_or(u64::MAX);
        elapsed.max(self.last_at_ms)
    }

    /// Grants `requested` lag credit to `player`, clamped per input and in total
    /// (TMR-11); returns the clamped value.
    fn clamp_lag(&mut self, player: PlayerId, requested: u32) -> u32 {
        let existing = self
            .lag_credit
            .iter()
            .position(|(credited, _)| *credited == player);
        let index = match existing {
            Some(index) => index,
            None => {
                self.lag_credit.push((player, 0));
                self.lag_credit.len() - 1
            }
        };
        let used = self.lag_credit[index].1;
        let granted = requested
            .min(LAG_CAP_MS)
            .min(LAG_TOTAL_CAP_MS.saturating_sub(used));
        self.lag_credit[index].1 = used.saturating_add(granted);
        granted
    }

    /// Feeds one participant input to the game (GAM-6, TMR-1, TMR-11).
    ///
    /// A rejected input (not in the round, not a participant, or left) is a
    /// [`RoundInputError`]; a game abort is reported by [`RoundStep::aborted`]
    /// (TMR-4, PRS-5, PRS-7, FLR-4).
    pub fn submit_input(
        &mut self,
        received_at: Instant,
        player: PlayerId,
        action: GameAction,
        lag_credit_ms: u32,
    ) -> Result<RoundStep, RoundInputError> {
        if self.ended() || received_at >= self.deadline {
            return Err(RoundInputError::NotInRound);
        }
        if !self.participants.contains(&player) {
            return Err(RoundInputError::NotParticipant);
        }
        if self.left.contains(&player) {
            return Err(RoundInputError::PlayerLeftRound);
        }
        let at_ms = self.at_ms(received_at);
        let lag_credit_ms = self.clamp_lag(player, lag_credit_ms);
        let input = GameInput::Player {
            player,
            action,
            lag_credit_ms,
        };
        Ok(self.step(at_ms, input))
    }

    /// Ticks the game with `TimeAdvanced` at `received_at`, the wake-up path
    /// (TMR-2, GAM-6). A no-op once the round is over.
    pub fn tick_advanced(&mut self, received_at: Instant) -> RoundStep {
        // TMR-4: the deadline is a hard cap, so never tick at or after it; the
        // actor ends the round by calling `finish`.
        if self.ended() || received_at >= self.deadline {
            return RoundStep::default();
        }
        let at_ms = self.at_ms(received_at);
        self.step(at_ms, GameInput::TimeAdvanced)
    }

    /// A participant's connection changed mid-round (PRS-6, PRS-11).
    ///
    /// A disconnect reports `Away` and starts the grace; a reconnect inside the
    /// grace reports `Present`. A repeated disconnect does not extend the grace
    /// or report twice (PRS-6).
    pub fn on_connection(
        &mut self,
        player: PlayerId,
        connected: bool,
        now: Instant,
    ) -> Vec<RoundEvent> {
        // TMR-4: past the deadline the round is over, so presence cannot change.
        if self.ended() || now >= self.deadline {
            return Vec::new();
        }
        if !self.participants.contains(&player) || self.left.contains(&player) {
            return Vec::new();
        }
        if connected {
            if self.away.contains(&player) && self.grace.returned(player, now) {
                self.away.retain(|away| *away != player);
                return vec![RoundEvent::PlayerStatus {
                    player,
                    status: PlayerStatus::Present,
                }];
            }
            Vec::new()
        } else if self.away.contains(&player) {
            Vec::new()
        } else {
            self.away.push(player);
            self.grace.away(player, now);
            vec![RoundEvent::PlayerStatus {
                player,
                status: PlayerStatus::Away,
            }]
        }
    }

    /// Expires every reconnect grace due at `now`: each expired participant is
    /// marked left exactly once and ticked `PlayerLeft` (TMR-2, PRS-6).
    ///
    /// After this their inputs are rejected with
    /// [`RoundInputError::PlayerLeftRound`] (PRS-7). If every participant is out,
    /// the round ends here ([`all_left`](Self::all_left), RND-13).
    pub fn expire(&mut self, now: Instant) -> RoundStep {
        // TMR-4: at or past the deadline the round ends, so no late `PlayerLeft`.
        if self.ended() || now >= self.deadline {
            return RoundStep::default();
        }
        let at_ms = self.at_ms(now);
        let mut events = Vec::new();
        for player in self.grace.expire(now) {
            self.away.retain(|away| *away != player);
            self.left.push(player);
            let step = self.step(at_ms, GameInput::PlayerLeft(player));
            events.extend(step.events);
            if step.aborted || step.finished || self.all_left() {
                return RoundStep {
                    events,
                    finished: step.finished,
                    aborted: step.aborted,
                };
            }
        }
        RoundStep {
            events,
            finished: false,
            aborted: false,
        }
    }

    /// Runs one input through the game and reports it as a [`RoundStep`].
    fn step(&mut self, at_ms: u64, input: GameInput) -> RoundStep {
        match self.tick(at_ms, input) {
            Ok(ticked) => RoundStep {
                events: ticked.events.into_iter().map(RoundEvent::Game).collect(),
                finished: ticked.finished,
                aborted: false,
            },
            Err(_) => RoundStep {
                events: Vec::new(),
                finished: false,
                aborted: true,
            },
        }
    }

    /// Feeds one input to the game (GAM-6, GAM-8) and reports whether the round
    /// now ends on `is_finished()` (TMR-3).
    ///
    /// `at_ms` is clamped so it never decreases (TMR-1). A `Fatal` error or a
    /// panic in `tick` aborts the round with no retry (FLR-4, FLR-7) and is not
    /// recorded; a tick that ran is recorded even when `is_finished` fails
    /// afterwards (REP-1).
    pub fn tick(&mut self, at_ms: u64, input: GameInput) -> Result<Ticked, SkipReason> {
        let at_ms = at_ms.max(self.last_at_ms);
        self.last_at_ms = at_ms;
        let outcome = catch_unwind(AssertUnwindSafe(|| self.game.tick(at_ms, input.clone())));
        match outcome {
            Ok(Ok(events)) => {
                // REP-1: the tick ran, so record it before `is_finished` can
                // fail; an aborted round still replays the inputs that ran.
                if let Some(recorder) = self.recorder.as_mut() {
                    recorder.record(at_ms, input);
                }
                let finished = match catch_unwind(AssertUnwindSafe(|| self.game.is_finished())) {
                    Ok(finished) => finished,
                    Err(_) => {
                        self.aborted = true;
                        return Err(SkipReason::GameAborted);
                    }
                };
                self.finished = finished;
                Ok(Ticked { events, finished })
            }
            Ok(Err(_)) | Err(_) => {
                self.aborted = true;
                Err(SkipReason::GameAborted)
            }
        }
    }

    /// Ends the round: calls `result()` exactly once, validates it (RND-8,
    /// RND-9), scores it (SCR-2..SCR-6, SCR-10, SCR-11) and builds the
    /// [`RoundRecord`].
    ///
    /// `removed_this_round` is the caller's PRS-3 list. `left` is merged with
    /// the runner's own tracked `PlayerLeft` set (seat order, unique), so a
    /// missing outcome becomes `NoResult(LeftRound)` rather than
    /// `NoResult(TimerExpired)` even when the caller passes `&[]` (RND-9,
    /// PRS-6). Points are added to `states` only for a `Scored` round (SCR-10).
    ///
    /// A round that aborted (a `Fatal` error or a panic) skips `result()` and
    /// becomes `Skipped(GameAborted)` (FLR-4, FLR-7). The finished flag is the
    /// cached value from the last successful tick, never a live `is_finished()`,
    /// so a panicking callback cannot slip an incomplete report through (TMR-3,
    /// FLR-7). The scoring mode was cached before play, so a live game is never
    /// queried here (SCR-1).
    pub fn finish(
        self,
        states: &mut [PlayerState],
        round_index: u32,
        removed_this_round: Vec<PlayerId>,
        left: &[PlayerId],
        record_replays: bool,
    ) -> RoundRecord {
        let Self {
            game,
            game_id,
            participants,
            attempts,
            scoring_mode,
            recorder,
            aborted,
            finished,
            left: tracked_left,
            ..
        } = self;
        let replay = replay_log(recorder, record_replays);
        // RND-9, PRS-6: the tracked set always counts, so a caller that forgot
        // to pass `PlayerLeft` still records `NoResult(LeftRound)`.
        let left = merge_left(&participants, &tracked_left, left);

        // FLR-4: a callback already failed, so there is no report to collect.
        if aborted {
            return skipped_record(
                round_index,
                Some(game_id),
                participants,
                attempts,
                SkipReason::GameAborted,
                removed_this_round,
                replay,
            );
        }

        let report = match catch_unwind(AssertUnwindSafe(|| game.result())) {
            Ok(report) => report,
            // A panic in `result()` is a failure, not a report to score (FLR-7).
            Err(_) => {
                return skipped_record(
                    round_index,
                    Some(game_id),
                    participants,
                    attempts,
                    SkipReason::InvalidReport,
                    removed_this_round,
                    replay,
                );
            }
        };
        // TMR-3, FLR-7: use the cached flag from the last tick; a live
        // `is_finished()` here would turn a panic into a silent `false`.
        if validate_report(&report.outcomes, &participants, scoring_mode, finished).is_err() {
            return skipped_record(
                round_index,
                Some(game_id),
                participants,
                attempts,
                SkipReason::InvalidReport,
                removed_this_round,
                replay,
            );
        }

        // RND-9: fill every participant the game omitted.
        let outcomes: Vec<PlayerOutcome> = participants
            .iter()
            .map(|&player| match outcome_of(&report.outcomes, player) {
                Some(outcome) => outcome,
                None => filled_outcome(fill_missing_outcome(false, left.contains(&player))),
            })
            .collect();

        if outcomes
            .iter()
            .all(|outcome| matches!(outcome, PlayerOutcome::NoResult(_)))
        {
            // SCR-9: every participant has no result, so nothing is scored. The
            // per-player reasons are still recorded for the round (RND-9).
            let round_disqualified = participants
                .iter()
                .zip(&outcomes)
                .filter_map(|(&player, outcome)| match outcome {
                    PlayerOutcome::NoResult(reason) => Some((player, *reason)),
                    _ => None,
                })
                .collect();
            return RoundRecord {
                round_index,
                game: Some(game_id),
                participants,
                attempts,
                status: RoundStatus::Skipped(SkipReason::NoResults),
                placements: Vec::new(),
                round_disqualified,
                removed_this_round,
                replay,
            };
        }

        let n = participants.len();
        let places = compute_places(&outcomes);
        let mut placements = Vec::with_capacity(n);
        let mut round_disqualified = Vec::new();
        for (index, &player) in participants.iter().enumerate() {
            let (place, group) = places[index];
            let points = points_for_place(n, place, group);
            if let Some(state) = states.iter_mut().find(|state| state.player.id == player) {
                state.total_points = state.total_points.saturating_add(points);
            }
            if let PlayerOutcome::NoResult(reason) = outcomes[index] {
                round_disqualified.push((player, reason));
            }
            placements.push((player, place as u8, points));
        }

        RoundRecord {
            round_index,
            game: Some(game_id),
            participants,
            attempts,
            status: RoundStatus::Scored,
            placements,
            round_disqualified,
            removed_this_round,
            replay,
        }
    }
}

/// Participants in first-mover order (RND-11): `players[0]` moves first, so the
/// seat named by RNG-11 is swapped to the front.
fn first_mover_order(participants: &[PlayerId], first_mover_seat: usize) -> Vec<PlayerId> {
    let mut players = participants.to_vec();
    if first_mover_seat < players.len() {
        players.swap(0, first_mover_seat);
    }
    players
}

/// The union of the runner's tracked `PlayerLeft` set and the caller's list,
/// in participant seat order and without duplicates (RND-9, PRS-6).
fn merge_left(
    participants: &[PlayerId],
    tracked: &[PlayerId],
    supplied: &[PlayerId],
) -> Vec<PlayerId> {
    participants
        .iter()
        .filter(|player| tracked.contains(*player) || supplied.contains(*player))
        .copied()
        .collect()
}

/// The `NoResult` reason for a participant the game did not report (RND-9).
fn filled_outcome(missing: MissingOutcome) -> PlayerOutcome {
    match missing {
        MissingOutcome::LeftRound => PlayerOutcome::NoResult(DisqualificationReason::LeftRound),
        // `Keep` is impossible here: the caller only asks when no outcome was
        // reported, but a `TimerExpired` fill is the safe default.
        MissingOutcome::Keep | MissingOutcome::TimerExpired => {
            PlayerOutcome::NoResult(DisqualificationReason::TimerExpired)
        }
    }
}

/// RND-9 validation: every entry names a participant, no duplicates, each
/// outcome fits the game's `ScoringMode`, `Placed(v)` is in `1..=n`, and a
/// finished game reported every participant.
fn validate_report(
    outcomes: &[(PlayerId, PlayerOutcome)],
    participants: &[PlayerId],
    scoring_mode: ScoringMode,
    finished: bool,
) -> Result<(), ()> {
    let n = participants.len();
    for (index, (player, outcome)) in outcomes.iter().enumerate() {
        if !participants.contains(player) {
            return Err(()); // a non-participant cannot be scored
        }
        if outcomes[..index].iter().any(|(seen, _)| seen == player) {
            return Err(()); // a duplicate would score twice
        }
        match outcome {
            PlayerOutcome::Placed(place) => {
                if scoring_mode != ScoringMode::Placement || *place == 0 || usize::from(*place) > n
                {
                    return Err(());
                }
            }
            PlayerOutcome::Scored(_) => {
                if scoring_mode != ScoringMode::HigherIsBetter {
                    return Err(());
                }
            }
            PlayerOutcome::NoResult(_) => {}
        }
    }
    if finished
        && participants
            .iter()
            .any(|player| outcome_of(outcomes, *player).is_none())
    {
        return Err(()); // GAM-7: a finished game reports every participant
    }
    Ok(())
}

/// A `RoundRecord` for a slot that ended without scoring (TYP-1, SCR-9, RND-9).
fn skipped_record(
    round_index: u32,
    game: Option<GameId>,
    participants: Vec<PlayerId>,
    attempts: u8,
    reason: SkipReason,
    removed_this_round: Vec<PlayerId>,
    replay: Option<ReplayLog>,
) -> RoundRecord {
    RoundRecord {
        round_index,
        game,
        participants,
        attempts,
        status: RoundStatus::Skipped(reason),
        placements: Vec::new(),
        round_disqualified: Vec::new(),
        removed_this_round,
        replay,
    }
}

/// The recorded log, only when replay recording is on (REP-1, REP-3).
fn replay_log(recorder: Option<ReplayRecorder>, record_replays: bool) -> Option<ReplayLog> {
    if record_replays {
        recorder.map(ReplayRecorder::into_log)
    } else {
        None
    }
}
