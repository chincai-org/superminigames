//! A scripted, deterministic `Game` for engine tests (T2.4, GAM-1 to GAM-8).
//!
//! Every trait method is pure, works purely from the script and performs no
//! I/O (GAM-2). A configured [`InitReply::Panic`] or `TickScript::panic` is the
//! only panic, so tests can drive the engine's retry and abort paths (FLR-1,
//! FLR-2, FLR-4). A later M2 task can build a full match scenario from this
//! single fake without writing another one.

use superminigames_core::game::{
    Game, GameError, GameEvent, GameInput, GameResult, Payload, ScoringMode, TimeModel,
};
use superminigames_core::ids::{GameId, PlayerId};
use superminigames_core::outcome::PlayerOutcome;

/// What one scripted `initialize` call does (GAM-4, GAM-5, FLR-1, FLR-2).
///
/// The script is a queue: each `initialize` consumes its front entry, whether
/// it succeeds, fails or panics. An empty queue falls back to the honest
/// behaviour: `Ok(())` when `supports_players` accepts the slice, otherwise
/// `GameError::InitFailed`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InitReply {
    /// `initialize` succeeds.
    Ok,
    /// `initialize` returns `GameError::InitFailed` (FLR-1, RND-5).
    Fail(String),
    /// `initialize` panics, as a buggy game would (FLR-2).
    Panic,
}

/// One scripted `tick` reply.
#[derive(Clone, Debug, Default)]
pub struct TickScript {
    /// The input this reply answers, or `None` for any input.
    pub input: Option<GameInput>,
    /// Events returned for the matching input (ignored for `panic`/`fatal`).
    pub events: Vec<GameEvent>,
    /// Panic instead of returning; the engine must catch it (FLR-4).
    pub panic: bool,
    /// Return `GameError::Fatal` instead of events (FLR-4).
    pub fatal: Option<String>,
    /// Flip `is_finished()` to `true` (GAM-8).
    pub finish: bool,
}

impl TickScript {
    /// A reply for exactly `input`.
    pub fn for_input(input: GameInput) -> Self {
        Self {
            input: Some(input),
            ..Self::default()
        }
    }

    /// A reply for any input.
    pub fn for_any() -> Self {
        Self::default()
    }

    /// The events this reply returns.
    pub fn events(mut self, events: Vec<GameEvent>) -> Self {
        self.events = events;
        self
    }

    /// Make this reply panic (FLR-4).
    pub fn panics(mut self) -> Self {
        self.panic = true;
        self
    }

    /// Make this reply a `Fatal` error (FLR-4).
    pub fn fatal(mut self, reason: impl Into<String>) -> Self {
        self.fatal = Some(reason.into());
        self
    }

    /// Make this reply finish the game (GAM-8).
    pub fn finishes(mut self) -> Self {
        self.finish = true;
        self
    }
}

/// A `Broadcast` event carrying `bytes` (TYP-1, PRO-9).
pub fn broadcast(bytes: impl Into<Vec<u8>>) -> GameEvent {
    GameEvent::Broadcast(Payload(bytes.into()))
}

/// A `ToPlayer` event carrying `bytes` (TYP-1, PRO-9).
pub fn to_player(player: PlayerId, bytes: impl Into<Vec<u8>>) -> GameEvent {
    GameEvent::ToPlayer {
        player,
        payload: Payload(bytes.into()),
    }
}

/// An `InputRejected` event (GAM-5, GAM-13).
pub fn rejected(player: PlayerId, reason: impl Into<String>) -> GameEvent {
    GameEvent::InputRejected {
        player,
        reason: reason.into(),
    }
}

/// How `result()` answers (GAM-7, RND-9).
#[derive(Clone, Debug)]
enum ResultBehavior {
    /// `Placed(seat + 1)` for every participant once finished, empty before.
    Placement,
    /// `Scored(score)` per seat, always, so unfinished players get a
    /// provisional outcome (GAM-7).
    Scored(Vec<i64>),
    /// The exact `GameResult` to return, so a test can omit a participant,
    /// repeat one or report an out-of-range place (RND-9).
    Fixed(GameResult),
}

/// A one-shot gate for the T-PRS-9 hook (test-only).
///
/// A registry factory is a plain `fn`, which cannot capture channels, so a test
/// parks one gate in a `OnceLock` and the factory reads it back by reference.
/// `initialize` signals `started` and then rendezvouses on `release`, which the
/// test reaches only when it is ready to let `initialize` finish.
pub struct InitGate {
    started: tokio::sync::mpsc::UnboundedSender<()>,
    release: std::sync::Arc<std::sync::Barrier>,
}

impl InitGate {
    /// A gate that signals on `started` and then waits for the test at
    /// `release`.
    pub fn new(
        started: tokio::sync::mpsc::UnboundedSender<()>,
        release: std::sync::Arc<std::sync::Barrier>,
    ) -> Self {
        Self { started, release }
    }

    /// Announces an in-flight `initialize` and blocks until it is released.
    fn signal_and_wait(&self) {
        let _ = self.started.send(());
        let _ = self.release.wait();
    }
}

/// A scripted, deterministic `Game` (T2.4, GAM-1).
pub struct FakeGame {
    id: GameId,
    name: &'static str,
    scoring_mode: ScoringMode,
    time_model: TimeModel,
    supported_players: Vec<usize>,
    init_script: Vec<InitReply>,
    tick_script: Vec<TickScript>,
    finish_after_ticks: Option<u32>,
    /// Wake-ups indexed by elapsed ticks (GAM-6): `next_wakeup_ms` serves
    /// entry `n` while exactly `n` ticks have run, then `None` past the end.
    wakeups: Vec<u64>,
    /// When set, `initialize` signals that it is in flight and then blocks
    /// until the test releases it (T-PRS-9). `None` keeps `initialize`
    /// immediate.
    init_gate: Option<&'static InitGate>,
    result_behavior: ResultBehavior,

    /// Every `(seed, players, time_limit_ms)` passed to `initialize`, in
    /// order, including failed and panicking attempts (RNG-6, FLR-2).
    initialize_attempts: Vec<(u64, Vec<PlayerId>, u64)>,

    /// `(seed, result)` overrides: when `initialize` sees that seed, `result()`
    /// reports exactly that `GameResult` (T-MTH-2).
    seed_results: Vec<(u64, GameResult)>,

    // Runtime state, fully reset by `initialize` (GAM-4).
    players: Vec<PlayerId>,
    seed: u64,
    time_limit_ms: u64,
    ticks: u32,
    clock_ms: u64,
    initialized: bool,
    finished: bool,
    initialize_calls: u32,
    tick_calls: u32,
}

impl FakeGame {
    /// Starts building a fake with the given id and display name.
    pub fn builder(id: &str, name: &'static str) -> FakeGameBuilder {
        FakeGameBuilder {
            game: FakeGame {
                id: GameId(id.to_string()),
                name,
                scoring_mode: ScoringMode::Placement,
                time_model: TimeModel::RoundDeadlineOnly,
                supported_players: vec![2],
                init_script: Vec::new(),
                tick_script: Vec::new(),
                finish_after_ticks: None,
                wakeups: Vec::new(),
                init_gate: None,
                result_behavior: ResultBehavior::Placement,
                initialize_attempts: Vec::new(),
                seed_results: Vec::new(),
                players: Vec::new(),
                seed: 0,
                time_limit_ms: 0,
                ticks: 0,
                clock_ms: 0,
                initialized: false,
                finished: false,
                initialize_calls: 0,
                tick_calls: 0,
            },
        }
    }

    /// How many times `initialize` was called.
    pub fn initialize_calls(&self) -> u32 {
        self.initialize_calls
    }

    /// How many times `tick` was called, including ticks after the finish.
    pub fn tick_calls(&self) -> u32 {
        self.tick_calls
    }

    /// The seed passed to the last `initialize` (GAM-3).
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// The participants passed to the last `initialize`.
    pub fn players(&self) -> &[PlayerId] {
        &self.players
    }

    /// The `at_ms` of the last `tick` (GAM-3).
    pub fn clock_ms(&self) -> u64 {
        self.clock_ms
    }

    /// The round time limit passed to the last `initialize` (TMR-1, TMR-11).
    pub fn time_limit_ms(&self) -> u64 {
        self.time_limit_ms
    }

    /// Every `(seed, players, time_limit_ms)` passed to `initialize`, in call
    /// order, including failed and panicking attempts (RNG-6, FLR-2).
    pub fn initialize_attempts(&self) -> &[(u64, Vec<PlayerId>, u64)] {
        &self.initialize_attempts
    }
}

/// Builds a [`FakeGame`] (T2.4).
pub struct FakeGameBuilder {
    game: FakeGame,
}

impl FakeGameBuilder {
    /// How the game interprets its outcomes (TYP-1, SCR-1).
    pub fn scoring_mode(mut self, mode: ScoringMode) -> Self {
        self.game.scoring_mode = mode;
        self
    }

    /// Where the game keeps its time (TMR-9).
    pub fn time_model(mut self, model: TimeModel) -> Self {
        self.game.time_model = model;
        self
    }

    /// The player counts `supports_players` accepts (GAM-5, PLL-1).
    pub fn supported_players(mut self, counts: impl Into<Vec<usize>>) -> Self {
        self.game.supported_players = counts.into();
        self
    }

    /// The scripted `initialize` replies, tried in order (FLR-1, FLR-2).
    pub fn init_script(mut self, replies: impl Into<Vec<InitReply>>) -> Self {
        self.game.init_script = replies.into();
        self
    }

    /// Appends one `initialize` reply to the script.
    pub fn init_reply(mut self, reply: InitReply) -> Self {
        self.game.init_script.push(reply);
        self
    }

    /// The scripted `tick` replies, tried in order.
    pub fn tick_script(mut self, scripts: impl Into<Vec<TickScript>>) -> Self {
        self.game.tick_script = scripts.into();
        self
    }

    /// Appends one `tick` reply to the script.
    pub fn tick_reply(mut self, script: TickScript) -> Self {
        self.game.tick_script.push(script);
        self
    }

    /// Finishes the game once `tick` has been called `ticks` times (GAM-8).
    pub fn finish_after_ticks(mut self, ticks: u32) -> Self {
        self.game.finish_after_ticks = Some(ticks);
        self
    }

    /// `next_wakeup_ms` returns entry `n` while `n` ticks have run, then
    /// `None` past the end (GAM-6).
    pub fn wakeups(mut self, wakeups: impl Into<Vec<u64>>) -> Self {
        self.game.wakeups = wakeups.into();
        self
    }

    /// Makes each `initialize` signal `init_started` and block on `init_release`
    /// before returning, so a test can drive the engine while initialization is
    /// still in flight (T-PRS-9). `None` keeps `initialize` immediate.
    #[must_use]
    pub fn block_init(mut self, gate: Option<&'static InitGate>) -> Self {
        self.game.init_gate = gate;
        self
    }
    /// `result()` returns a provisional `Scored(score)` for every seat
    /// (GAM-7).
    pub fn scored(mut self, scores: impl Into<Vec<i64>>) -> Self {
        self.game.result_behavior = ResultBehavior::Scored(scores.into());
        self
    }

    /// `result()` returns exactly `result` (RND-9).
    pub fn result(mut self, result: GameResult) -> Self {
        self.game.result_behavior = ResultBehavior::Fixed(result);
        self
    }

    /// `result()` reports exactly `result` for the round whose `initialize`
    /// seed is `seed` (T-MTH-2). Rounds whose seed is not listed keep the
    /// behaviour set by `result`/`scored`/the default.
    pub fn result_for_seed(mut self, seed: u64, result: GameResult) -> Self {
        self.game.seed_results.push((seed, result));
        self
    }

    /// The finished fake game.
    pub fn build(self) -> FakeGame {
        self.game
    }
}

impl Game for FakeGame {
    fn id(&self) -> GameId {
        self.id.clone()
    }

    fn name(&self) -> &'static str {
        self.name
    }

    fn scoring_mode(&self) -> ScoringMode {
        self.scoring_mode
    }

    fn time_model(&self) -> TimeModel {
        self.time_model
    }

    fn supports_players(&self, count: usize) -> bool {
        self.supported_players.contains(&count)
    }

    fn initialize(
        &mut self,
        seed: u64,
        players: &[PlayerId],
        time_limit_ms: u64,
    ) -> Result<(), GameError> {
        self.initialize_calls += 1;
        // GAM-4: reset every field a previous round could have touched.
        self.players = players.to_vec();
        self.seed = seed;
        self.time_limit_ms = time_limit_ms;
        // RNG-6, FLR-2: keep every attempt so retries can be checked.
        self.initialize_attempts
            .push((seed, players.to_vec(), time_limit_ms));
        // T-PRS-9 test hook: announce the in-flight initialize and wait for the
        // test to release it. The gate lives for the whole test process.
        if let Some(gate) = self.init_gate {
            gate.signal_and_wait();
        }
        self.ticks = 0;
        self.clock_ms = 0;
        self.initialized = false;
        self.finished = false;
        // T-MTH-2: a scripted per-round outcome, selected by the seed the
        // engine derived for this round (RNG-10).
        if let Some((_, result)) = self.seed_results.iter().find(|(want, _)| *want == seed) {
            self.result_behavior = ResultBehavior::Fixed(result.clone());
        }
        let reply = if self.init_script.is_empty() {
            None
        } else {
            Some(self.init_script.remove(0))
        };
        match reply {
            Some(InitReply::Panic) => panic!("FakeGame: scripted initialize panic"),
            Some(InitReply::Fail(reason)) => Err(GameError::InitFailed(reason)),
            Some(InitReply::Ok) => {
                self.initialized = true;
                Ok(())
            }
            None if self.supports_players(players.len()) => {
                self.initialized = true;
                Ok(())
            }
            None => Err(GameError::InitFailed(format!(
                "{} does not support {} players",
                self.name,
                players.len()
            ))),
        }
    }

    fn tick(&mut self, at_ms: u64, input: GameInput) -> Result<Vec<GameEvent>, GameError> {
        self.tick_calls += 1;
        self.clock_ms = at_ms;
        if self.finished {
            return Ok(Vec::new()); // GAM-8: inputs after the end are ignored.
        }
        if !self.initialized {
            return Err(GameError::Fatal("tick before initialize".to_string()));
        }
        self.ticks += 1;

        let matched = self
            .tick_script
            .iter()
            .position(|script| script.input.as_ref().is_none_or(|want| want == &input));
        if let Some(script) = matched.map(|index| self.tick_script.remove(index)) {
            if script.panic {
                panic!("FakeGame: scripted tick panic");
            }
            if let Some(reason) = script.fatal {
                return Err(GameError::Fatal(reason));
            }
            if script.finish {
                self.finished = true;
            }
            return Ok(script.events);
        }
        if self.finish_after_ticks == Some(self.ticks) {
            self.finished = true;
        }
        Ok(Vec::new())
    }

    fn next_wakeup_ms(&self) -> Option<u64> {
        if self.finished {
            return None;
        }
        self.wakeups.get(self.ticks as usize).copied()
    }

    fn is_finished(&self) -> bool {
        self.finished
    }

    fn result(&self) -> GameResult {
        match &self.result_behavior {
            ResultBehavior::Placement => {
                if !self.finished {
                    return GameResult {
                        outcomes: Vec::new(),
                    };
                }
                GameResult {
                    outcomes: self
                        .players
                        .iter()
                        .enumerate()
                        .map(|(seat, player)| (*player, PlayerOutcome::Placed((seat + 1) as u8)))
                        .collect(),
                }
            }
            ResultBehavior::Scored(scores) => GameResult {
                outcomes: self
                    .players
                    .iter()
                    .enumerate()
                    .filter_map(|(seat, player)| {
                        scores
                            .get(seat)
                            .map(|score| (*player, PlayerOutcome::Scored(*score)))
                    })
                    .collect(),
            },
            ResultBehavior::Fixed(result) => result.clone(),
        }
    }
}
