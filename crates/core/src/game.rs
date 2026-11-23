//! The game contract (TYP-1, section 13).

use serde::{Deserialize, Serialize};

use crate::ids::{GameId, PlayerId};
use crate::outcome::PlayerOutcome;

/// How a game's outcomes are interpreted (TYP-1, SCR-1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScoringMode {
    /// The game reports final places (`Placed`).
    Placement,
    /// The game reports raw scores (`Scored`); higher ranks better.
    HigherIsBetter,
}

/// Where a game keeps its time (TYP-1, TMR-9).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeModel {
    /// The game keeps per-player clocks (chess, doushouqi).
    PlayerClocks,
    /// Only the round deadline limits play.
    RoundDeadlineOnly,
}

/// Opaque action payload in the game's own compact binary encoding (TYP-1).
///
/// The game validates it (GAM-4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameAction(#[serde(with = "serde_bytes")] pub Vec<u8>);

/// One input to a running game (TYP-1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GameInput {
    Player {
        player: PlayerId,
        action: GameAction,
        /// Server-measured lag credit, already clamped by the engine (TMR-11).
        lag_credit_ms: u32,
    },
    /// Sent by the engine when a participant leaves mid-round (PRS-6).
    PlayerLeft(PlayerId),
    /// Sent when the game's `next_wakeup_ms` is reached.
    TimeAdvanced,
}

/// Opaque event payload in the game's own compact binary encoding (TYP-1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Payload(#[serde(with = "serde_bytes")] pub Vec<u8>);

/// Something a game reports back to the engine (TYP-1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GameEvent {
    /// Visible to all participants.
    Broadcast(Payload),
    /// Private to one player.
    ToPlayer { player: PlayerId, payload: Payload },
    /// The input was rejected by the game's own rules (GAM-5).
    InputRejected { player: PlayerId, reason: String },
}

/// What a game reports for every participant once a round ends (TYP-1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameResult {
    pub outcomes: Vec<(PlayerId, PlayerOutcome)>,
}

/// Why a game could not run (TYP-1, RND-5).
#[derive(Debug, thiserror::Error)]
pub enum GameError {
    /// The engine MAY retry the slot (RND-5).
    #[error("game failed to initialize: {0}")]
    InitFailed(String),
    /// No retry (RND-5).
    #[error("game failed during play: {0}")]
    Fatal(String),
}

/// The contract every game crate implements (GAM-1 to GAM-12).
pub trait Game: Send {
    fn id(&self) -> GameId;
    fn name(&self) -> &'static str;
    fn scoring_mode(&self) -> ScoringMode;
    fn time_model(&self) -> TimeModel;
    fn supports_players(&self, count: usize) -> bool;
    /// Set up a fresh round. MUST fully reset all state (GAM-2).
    fn initialize(
        &mut self,
        seed: u64,
        players: &[PlayerId],
        time_limit_ms: u64,
    ) -> Result<(), GameError>;
    /// `at_ms` = milliseconds since round start, measured by the engine (GAM-3).
    fn tick(&mut self, at_ms: u64, input: GameInput) -> Result<Vec<GameEvent>, GameError>;
    /// Next moment (ms since round start) the game needs a tick even with no
    /// input, for example a clock running out (GAM-6).
    fn next_wakeup_ms(&self) -> Option<u64>;
    fn is_finished(&self) -> bool;
    /// Outcomes the game considers determined, or is willing to rank, right now
    /// (GAM-8, RND-9).
    fn result(&self) -> GameResult;
}

/// Each game crate exports a function of this type (GAM-1, TYP-2).
pub type GameFactory = fn() -> Box<dyn Game>;
