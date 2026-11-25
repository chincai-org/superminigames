//! Engine report and state value types (TYP-1, section 3).
//!
//! These types are engine-internal. They do not cross a crate or network
//! boundary, so they derive no `Serialize`/`Deserialize` (TYP-1, PRO-4); the
//! server owns the mapping to the protocol types (PRO-4). Only the derives the
//! engine needs are added.

use superminigames_core::config::MatchConfig;
use superminigames_core::game::GameInput;
use superminigames_core::ids::{GameId, LobbyId, MatchId, Player, PlayerId};
use superminigames_core::outcome::DisqualificationReason;

/// A player's standing in a match (PRS-3, PRS-4). `Removed` is permanent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    /// Still in the match; may be absent for a round and return later (PRS-5).
    InMatch,
    /// Removed by the absent-streak rule; never plays again (PRS-4).
    Removed,
}

/// One seated player's match state (TYP-1, ARC-1).
///
/// `ready` is meaningful in the lobby only. `connected` is changed only by
/// `Command::Connection` (PRS-1). `absent_streak` and `standing` change only at
/// a round start (PRS-2, PRS-3, PRS-8).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerState {
    pub player: Player,
    pub ready: bool,
    pub connected: bool,
    pub standing: Standing,
    pub absent_streak: u32,
    pub total_points: u32,
}

/// What a client may see of a lobby (TYP-1, PRO-14).
///
/// The join code itself is never included; only the `private` flag is (PRO-15).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LobbySnapshot {
    pub lobby_id: LobbyId,
    pub host: PlayerId,
    /// `(player, ready)` in seat order.
    pub seats: Vec<(Player, bool)>,
    pub config: MatchConfig,
    pub private: bool,
    pub settings_locked: bool,
}

/// Why a round slot was not scored (TYP-1, section 12.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipReason {
    /// Fewer than 2 participants (RND-3).
    NotEnoughPlayers,
    /// No eligible game (RND-4).
    EmptyPool,
    /// All `START_ATTEMPTS` attempts failed (FLR-3).
    StartFailed,
    /// Every participant ended with `NoResult` (SCR-9).
    NoResults,
    /// The game result failed validation (RND-9).
    InvalidReport,
    /// The game returned `GameError::Fatal` or panicked (FLR-4).
    GameAborted,
}

/// Whether a round scored points or was skipped (TYP-1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoundStatus {
    Scored,
    Skipped(SkipReason),
}

/// One round's full record, from the game down to the replay (TYP-1, REP-1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayLog {
    pub game: GameId,
    pub game_seed: u64,
    /// Participants in first-mover order (RND-11).
    pub players: Vec<PlayerId>,
    pub time_limit_ms: u64,
    /// `(at_ms, input)` in the exact order ticked (REP-1).
    pub inputs: Vec<(u64, GameInput)>,
}

/// What the engine reports about one round slot (TYP-1, MTH-3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoundRecord {
    pub round_index: u32,
    pub game: Option<GameId>,
    /// Participants in seat order (RND-3, RND-11).
    pub participants: Vec<PlayerId>,
    pub attempts: u8,
    pub status: RoundStatus,
    /// `(player, place, points)`, empty when `status` is `Skipped` (SCR-2).
    pub placements: Vec<(PlayerId, u8, u32)>,
    /// Round disqualifications only; not a `Standing` (PRS-8, section 12.3).
    pub round_disqualified: Vec<(PlayerId, DisqualificationReason)>,
    /// Players removed at this slot's round start (PRS-3).
    pub removed_this_round: Vec<PlayerId>,
    /// Present only when `EngineOptions.record_replays` is set (REP-3).
    pub replay: Option<ReplayLog>,
}

/// One player's line in the final ranking (TYP-1, FIN-1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RankEntry {
    pub player: PlayerId,
    /// Competition ranking; ties share a rank (FIN-1).
    pub rank: u32,
    pub total_points: u32,
    pub standing: Standing,
}

/// The complete result of a match (TYP-1, MTH-3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchReport {
    pub match_id: MatchId,
    pub lobby_id: LobbyId,
    pub config: MatchConfig,
    pub rounds: Vec<RoundRecord>,
    /// Every seated player, in seat order (FIN-1).
    pub final_ranking: Vec<RankEntry>,
    /// True if the end condition fired before all slots were processed (PRS-10).
    pub ended_early: bool,
}

/// A player's observable presence (TYP-1, PRS-6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerStatus {
    Present,
    Away,
    Removed,
}
