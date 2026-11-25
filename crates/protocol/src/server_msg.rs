//! Server to client messages (PRO-7, PRO-9, PRO-14, PRO-15).
//!
//! These are projections of engine state, not engine state itself: the seed, the
//! RNG state and replay logs never appear here (PRO-4, SRV-10), and a join code
//! appears only where a player already proved they may see it (PRO-15).

use serde::{Deserialize, Serialize};
use superminigames_core::game::Payload;
use superminigames_core::ids::{GameId, LobbyId, Player, PlayerId};

use crate::client_msg::LobbyConfig;

/// One seat in the lobby room (PRO-7, TYP-1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seat {
    /// The seated player.
    pub player: Player,
    /// Whether this player is ready (LOB-7).
    pub ready: bool,
}

/// The whole lobby, sent on every change (PRO-7, TYP-1).
///
/// The config is the code-free, seed-free [`LobbyConfig`], so a snapshot can
/// never leak the match seed before the match ends (SRV-10).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LobbySnapshot {
    /// Which lobby.
    pub lobby_id: LobbyId,
    /// Who may start and change settings (LOB-13).
    pub host: PlayerId,
    /// The occupied seats in join order (LOB-4).
    pub seats: Vec<Seat>,
    /// Current settings without the seed (PRO-16).
    pub config: LobbyConfig,
    /// Whether the lobby is unlisted; the code itself is never included (PRO-15).
    pub private: bool,
    /// True while any seat is ready, so settings edits are refused (LOB-6).
    pub settings_locked: bool,
}

/// One entry of the public lobby list (PRO-15).
///
/// It carries no join code, no seed and no player id, by construction (PRO-15).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicLobby {
    /// Which lobby; the only identifier a list entry may carry (PRO-15).
    pub lobby_id: LobbyId,
    /// The host's display name, never their id (PRO-15).
    pub host_name: String,
    /// Occupied seats (PRO-15).
    pub seats_taken: u8,
    /// Seats in total, `MAX_PLAYERS` today (SCP-4, PRO-15).
    pub max_seats: u8,
    /// Round slots (PRO-15).
    pub rounds: u32,
    /// Seconds per round (PRO-15).
    pub round_time_limit_secs: u64,
}

/// Where the player stands in the matchmaking queue (PRO-7, MAT-6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueState {
    /// Waiting for a partner (MAT-2).
    Queued,
    /// Not waiting any more, because the player left or was paired (MAT-6).
    NotQueued,
}

/// A player's place in a finished round (PRO-7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    /// The player.
    pub player: PlayerId,
    /// Final place, `1` is best; ties repeat the value (SCR-3).
    pub place: u8,
}

/// A points value for one player (PRO-7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerPoints {
    /// The player.
    pub player: PlayerId,
    /// Points at scale `POINTS_SCALE` (SCR-6), round points or match total.
    pub points: u32,
}

/// Why a round slot produced no score (PRO-7, TYP-1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// Fewer than two participants (RND-3).
    NotEnoughPlayers,
    /// No eligible game (RND-4).
    EmptyPool,
    /// Every start attempt failed (RND-5).
    StartFailed,
    /// Every participant ended with `NoResult` (RND-9).
    NoResults,
    /// The game's result failed validation (RND-9).
    InvalidReport,
    /// The game failed fatally or panicked (GAM-5).
    GameAborted,
}

/// The engine phase as a client sees it (PRO-14).
///
/// The names are the lowercase strings `Resync` carries (PRO-14), and the set is
/// the engine phase machine of section 12.1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Accepting players and settings (LOB-1).
    Lobby,
    /// The match is starting; no input is accepted yet (RND-1).
    Starting,
    /// A round is running (RND-1).
    InRound,
    /// A round ended and scores were applied (RND-10).
    RoundFinished,
    /// The match is over (FIN-2).
    MatchFinished,
    /// The lobby ended without a match (LOB-14).
    Cancelled,
}

/// A player's standing in the match; `Removed` is permanent (TYP-1, PRS-2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Standing {
    /// Still competing (PRS-1).
    InMatch,
    /// Removed by the absent-round rule; keeps their points and rank (PRS-3, FIN-1).
    Removed,
}

/// One line of the final ranking (PRO-7, FIN-1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RankEntry {
    /// The player.
    pub player: PlayerId,
    /// Competition rank; tied totals share a rank (FIN-1, DEC-60).
    pub rank: u32,
    /// Match total at scale `POINTS_SCALE` (SCR-10).
    pub total_points: u32,
    /// Whether the player was still in the match at the end (PRS-2).
    pub standing: Standing,
}

/// A player's transport-level presence (PRO-7, section 8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayerStatus {
    /// Connected (PRS-4).
    Present,
    /// Disconnected inside the reconnect grace (PRS-5).
    Away,
    /// Removed from the match (PRS-3).
    Removed,
}

/// Everything the server sends (PRO-7).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerMessage {
    /// The full lobby state after any change (PRO-7, PRO-14).
    LobbyState {
        /// The new state.
        snapshot: LobbySnapshot,
    },
    /// The public lobby list (PRO-7, PRO-15).
    PublicLobbies {
        /// Open public lobbies; private ones are never listed (SRV-5).
        lobbies: Vec<PublicLobby>,
    },
    /// The player's queue position (PRO-7, MAT-6).
    QueueStatus {
        /// Whether the player waits in a queue.
        state: QueueState,
        /// Seconds spent waiting, `0` when not queued (MAT-6).
        waited_secs: u64,
    },
    /// Matchmaking paired the player (PRO-7, MAT-3).
    MatchFound {
        /// The lobby both players were seated in.
        lobby_id: LobbyId,
        /// Seconds to accept, `15` today (MAT-3).
        accept_secs: u64,
    },
    /// A round slot started (PRO-7, RND-8).
    RoundStarting {
        /// Slot index from `0` (RND-2).
        round_index: u32,
        /// The selected game (RND-4).
        game_id: GameId,
        /// Display name of that game (GAM-1).
        game_name: String,
        /// Who plays this round, in first-mover order (RND-11).
        participants: Vec<PlayerId>,
        /// Seconds the round may run.
        time_limit_secs: u64,
    },
    /// A game event for this recipient (PRO-7, PRO-9).
    GameEvent {
        /// The game's own opaque encoding (GAM-5, PRO-1).
        payload: Payload,
    },
    /// A round was scored (PRO-7, RND-10).
    RoundScored {
        /// Slot index from `0`.
        round_index: u32,
        /// Places, one per participant (SCR-3).
        placements: Vec<Placement>,
        /// Points this round earned (SCR-6).
        points: Vec<PlayerPoints>,
    },

    /// A round was skipped (PRO-7, RND-6).
    RoundSkipped {
        /// Slot index from `0`.
        round_index: u32,
        /// Why it produced no score.
        reason: SkipReason,
    },
    /// Match totals after a round (PRO-7, SCR-10).
    ScoreUpdate {
        /// One entry per player still in the match.
        totals: Vec<PlayerPoints>,
    },
    /// A player's presence changed (PRO-7, PRS-4).
    PlayerStatus {
        /// Which player.
        player: PlayerId,
        /// Their new status.
        status: PlayerStatus,
    },
    /// The state a reconnecting client must adopt (PRO-7, PRO-14).
    Resync {
        /// The current phase.
        phase: Phase,
        /// The current or last processed slot (PRO-14).
        round_index: u32,
        /// Whether this player is playing the current round (PRO-14, PRS-7).
        is_participant: bool,
    },
    /// The match is over (PRO-7, FIN-2).
    MatchFinished {
        /// The final ranking (FIN-1).
        ranking: Vec<RankEntry>,
    },
    /// A rejection with a structured code (PRO-7, PRO-8).
    Error {
        /// What went wrong.
        code: crate::error_code::ErrorCode,
        /// Human-readable text only; clients branch on `code` (PRO-8).
        message: String,
    },
    /// Reply to `Ping`, echoing its nonce (PRO-7).
    Pong {
        /// The nonce from the `Ping`.
        nonce: u64,
    },
}
