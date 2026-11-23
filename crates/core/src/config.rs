//! Match configuration (TYP-1, section 4).
//!
//! Only the type definitions live here. Config validation guards (LOB-12) and
//! the lobby state machine belong to the engine (T2.2).

use serde::{Deserialize, Serialize};

use crate::ids::{GameId, Visibility};

/// Seats in a lobby (SCP-4).
pub const MIN_PLAYERS: usize = 2;
/// Seats in a lobby (SCP-4).
pub const MAX_PLAYERS: usize = 2;

/// Which games a lobby may select (TYP-1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GameFilterMode {
    /// Only these games are eligible.
    AllowList(Vec<GameId>),
    /// These games are excluded.
    BanList(Vec<GameId>),
}

/// The host's settings for a match (TYP-1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchConfig {
    /// Round slots, `1..=100` (MTH-1).
    pub rounds: u32,
    /// Wall-clock limit of one round, `1..=3600` seconds.
    pub round_time_limit_secs: u64,
    /// Drawn by the server at `StartMatch` (TYP-3, SRV-10); tests use 0.
    pub seed: u64,
    /// `K`, the absent round starts that remove a player; `1..=10` (PRS-3).
    pub max_absent_round_starts: u32,
    /// Which games may be selected.
    pub filter: GameFilterMode,
}

/// A partial settings change from the host (TYP-1, LOB-12).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsUpdate {
    pub rounds: Option<u32>,
    pub round_time_limit_secs: Option<u64>,
    pub max_absent_round_starts: Option<u32>,
    pub filter: Option<GameFilterMode>,
    pub visibility: Option<Visibility>,
}
