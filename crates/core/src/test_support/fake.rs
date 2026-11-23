//! A scripted fake game used to exercise the conformance suite (GAM-11).
//!
//! It is deliberately small but honest: it obeys GAM-1 to GAM-13 so the suite
//! can be checked against it before any real game exists. Its own encoding is
//! one byte (`0x01` move, `0x02` pass, `0x03` resign); anything else is
//! malformed and is rejected with `GameEvent::InputRejected`.

use rand_core::RngCore;

use super::super::game::{
    Game, GameAction, GameError, GameEvent, GameInput, GameResult, Payload, ScoringMode, TimeModel,
};
use super::super::ids::{GameId, PlayerId};
use super::super::outcome::PlayerOutcome;
use super::super::rng::chacha_from_seed;

/// The only well-formed action payloads of the fake game.
pub const MOVE: u8 = 0x01;
/// The only well-formed action payloads of the fake game.
pub const PASS: u8 = 0x02;
/// The only well-formed action payloads of the fake game.
pub const RESIGN: u8 = 0x03;

/// How a [`FakeGame`] behaves.
#[derive(Clone, Debug)]
pub struct FakeGameConfig {
    pub id: GameId,
    pub name: &'static str,
    pub scoring_mode: ScoringMode,
    pub time_model: TimeModel,
    /// Player counts `supports_players` accepts (GAM-5).
    pub supported_players: Vec<usize>,
    /// Number of `TimeAdvanced` ticks after which the game finishes; `None`
    /// means it never finishes on its own.
    pub ticks_to_finish: Option<u32>,
    /// Seat the game eliminates by its own rules while play continues (T-GAM-7).
    pub eliminated: Option<usize>,
    /// The place the eliminated seat is reported with.
    pub eliminated_place: u8,
    /// A `RoundDeadlineOnly` game scores every participant, so nobody is
    /// disqualified (GAM-7).
    pub score_at_deadline: bool,
}

impl Default for FakeGameConfig {
    fn default() -> Self {
        Self {
            id: GameId("fake".to_string()),
            name: "Fake Game",
            scoring_mode: ScoringMode::Placement,
            time_model: TimeModel::RoundDeadlineOnly,
            supported_players: vec![2, 3, 4],
            ticks_to_finish: None,
            eliminated: None,
            eliminated_place: 3,
            score_at_deadline: false,
        }
    }
}

/// A deterministic, dependency-free game for suite testing.
pub struct FakeGame {
    config: FakeGameConfig,
    players: Vec<PlayerId>,
    time_limit_ms: u64,
    ticks: u32,
    finished: bool,
    /// A seed-derived value, so equal seeds produce equal events (GAM-1, GAM-3).
    draw: u64,
    left: Vec<PlayerId>,
}

impl FakeGame {
    /// A game with the given behaviour.
    pub fn new(config: FakeGameConfig) -> Self {
        Self {
            config,
            players: Vec::new(),
            time_limit_ms: 0,
            ticks: 0,
            finished: false,
            draw: 0,
            left: Vec::new(),
        }
    }

    /// The action payload the fake game accepts for `byte` (GAM-6 helper).
    pub fn action(byte: u8) -> GameAction {
        GameAction(vec![byte])
    }

    /// `true` when the fake game rejects `action` as malformed.
    pub fn is_malformed(action: &GameAction) -> bool {
        decode(action).is_err()
    }

    fn seat_of(&self, player: PlayerId) -> Option<usize> {
        self.players.iter().position(|seated| *seated == player)
    }

    fn tick_once(&mut self) -> Vec<GameEvent> {
        // GAM-8: once finished, further inputs are ignored.
        if self.finished {
            return Vec::new();
        }
        self.ticks += 1;
        if self.config.ticks_to_finish == Some(self.ticks) {
            self.finished = true;
        }
        Vec::new()
    }

    /// Events carry `at_ms` and the seed-derived `draw`, so equal scripts
    /// produce equal events (GAM-1).
    fn broadcast(&self, at_ms: u64, player_index: usize, action: u8) -> GameEvent {
        let mut bytes = vec![action, player_index as u8];
        bytes.extend_from_slice(&self.draw.to_le_bytes());
        bytes.extend_from_slice(&at_ms.to_le_bytes());
        GameEvent::Broadcast(Payload(bytes))
    }

    fn score(&self, player_index: usize) -> i64 {
        // Deterministic in (draw, ticks, seat) and never a `NoResult` (GAM-7).
        (self.draw % 100) as i64 + i64::from(self.ticks) + player_index as i64
    }
}

/// The fake game's codec: exactly one byte in `0x01..=0x03` (GAM-6).
fn decode(action: &GameAction) -> Result<u8, String> {
    match action.0.as_slice() {
        [byte @ (MOVE | PASS | RESIGN)] => Ok(*byte),
        other => Err(format!(
            "expected exactly one byte in 0x01..=0x03, got {} byte(s)",
            other.len()
        )),
    }
}

impl Game for FakeGame {
    fn id(&self) -> GameId {
        self.config.id.clone()
    }

    fn name(&self) -> &'static str {
        self.config.name
    }

    fn scoring_mode(&self) -> ScoringMode {
        self.config.scoring_mode
    }

    fn time_model(&self) -> TimeModel {
        self.config.time_model
    }

    fn supports_players(&self, count: usize) -> bool {
        self.config.supported_players.contains(&count)
    }

    fn initialize(
        &mut self,
        seed: u64,
        players: &[PlayerId],
        time_limit_ms: u64,
    ) -> Result<(), GameError> {
        if !self.supports_players(players.len()) {
            return Err(GameError::InitFailed(format!(
                "{} does not support {} players",
                self.config.name,
                players.len()
            )));
        }
        // GAM-4: every field is reset, no reliance on a fresh instance.
        self.players = players.to_vec();
        self.time_limit_ms = time_limit_ms;
        self.ticks = 0;
        self.finished = false;
        self.left.clear();
        // GAM-3: all randomness comes from the seed through the shared helper.
        self.draw = chacha_from_seed(seed, 0).next_u64();
        Ok(())
    }

    fn tick(&mut self, at_ms: u64, input: GameInput) -> Result<Vec<GameEvent>, GameError> {
        if self.finished {
            return Ok(Vec::new());
        }
        if self.players.is_empty() {
            return Err(GameError::Fatal("tick before initialize".to_string()));
        }
        // GAM-2: time is read only from `at_ms` and `time_limit_ms`. The wakeup
        // schedule below is the only place `time_limit_ms` is needed.

        match input {
            GameInput::Player { player, action, .. } => {
                let Some(seat) = self.seat_of(player) else {
                    return Ok(vec![GameEvent::InputRejected {
                        player,
                        reason: "not a participant".to_string(),
                    }]);
                };
                match decode(&action) {
                    Err(reason) => Ok(vec![GameEvent::InputRejected { player, reason }]),
                    Ok(RESIGN) => {
                        self.finished = true;
                        Ok(vec![self.broadcast(at_ms, seat, RESIGN)])
                    }
                    Ok(byte) => {
                        let mut events = vec![self.broadcast(at_ms, seat, byte)];
                        events.extend(self.tick_once());
                        Ok(events)
                    }
                }
            }
            GameInput::PlayerLeft(player) => {
                // GAM-9: this game keeps the player in play and ranks them.
                if !self.left.contains(&player) {
                    self.left.push(player);
                }
                let mut events = self.tick_once();
                events.push(GameEvent::InputRejected {
                    player,
                    reason: "PlayerLeft has no reply".to_string(),
                });
                Ok(events)
            }
            GameInput::TimeAdvanced => Ok(self.tick_once()),
        }
    }

    fn next_wakeup_ms(&self) -> Option<u64> {
        // GAM-12: a `PlayerClocks` clock runs out at the round's time limit.
        if self.config.time_model != TimeModel::PlayerClocks
            || self.finished
            || self.time_limit_ms == 0
        {
            return None;
        }
        let next = (u64::from(self.ticks) + 1) * 1_000;
        if next >= self.time_limit_ms {
            None
        } else {
            Some(next)
        }
    }

    fn is_finished(&self) -> bool {
        self.finished
    }

    fn result(&self) -> GameResult {
        // GAM-7: pure, callable after `initialize`, and complete once finished.
        let mut outcomes = Vec::with_capacity(self.players.len());
        for (seat, player) in self.players.iter().enumerate() {
            let outcome = if self.config.score_at_deadline {
                Some(PlayerOutcome::Scored(self.score(seat)))
            } else if self.config.eliminated == Some(seat) {
                Some(PlayerOutcome::Placed(self.config.eliminated_place))
            } else if self.finished {
                Some(PlayerOutcome::Placed(if self.config.eliminated.is_some() {
                    1
                } else {
                    seat as u8 + 1
                }))
            } else {
                None
            };
            if let Some(outcome) = outcome {
                outcomes.push((*player, outcome));
            }
        }
        GameResult { outcomes }
    }
}
