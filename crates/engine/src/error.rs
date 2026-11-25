//! Engine errors (TYP-1).
//!
//! `LobbyError` describes why a lobby command was rejected (T2.2, section 4).
//! `InputError` and `EngineError` describe the match and actor, so they belong
//! to T2.6.

use superminigames_core::error::ConfigError;

/// Why a lobby command was rejected (TYP-1, LOB-1..LOB-16).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LobbyError {
    /// The command is not valid in the current phase (LOB-4, LOB-10, LOB-14).
    #[error("the command is not valid in the current phase")]
    WrongState,
    /// Only the host may run this command (LOB-6, LOB-9, LOB-10).
    #[error("only the host may do that")]
    NotHost,
    /// A join attempt when both seats are taken (LOB-2).
    #[error("the lobby is full")]
    LobbyFull,
    /// A private lobby join with a missing or wrong code (LOB-3).
    #[error("the join code is missing or wrong")]
    InvalidJoinCode,
    /// `UpdateSettings` while a seated player is Ready (LOB-6).
    #[error("settings are locked while a player is ready")]
    SettingsLocked,
    /// `StartMatch` while a seated player is not Ready (LOB-9).
    #[error("not every seated player is ready")]
    NotAllReady,
    /// `StartMatch` with fewer than `MIN_PLAYERS` seated (LOB-9).
    #[error("not enough players")]
    NotEnoughPlayers,
    /// A command from a player who is not seated (LOB-5, LOB-9).
    #[error("the player is not seated in this lobby")]
    UnknownPlayer,
    /// The settings failed validation (LOB-12).
    #[error("invalid settings: {0}")]
    Config(ConfigError),
}

/// Why an in-round game command was rejected (TYP-1, section 8).
///
/// The actor maps these to `EngineError::Input`; the round runner keeps its own
/// [`crate::round::RoundInputError`] because it has no actor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum InputError {
    /// The command arrived outside `Phase::InRound` (section 8).
    #[error("the match is not in a round")]
    NotInRound,
    /// The sender is not a participant in the running round (RND-3).
    #[error("the player is not a participant in this round")]
    NotParticipant,
    /// The sender's reconnect grace expired, so they are out for the round
    /// (PRS-7).
    #[error("the player left this round")]
    PlayerLeftRound,
}

/// A command's typed result (TYP-1, ARC-2, ARC-5).
///
/// The actor never panics on a rejected command: it answers with one of these
/// variants (FLR-8, ARC-5).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EngineError {
    /// A lobby or presence command was rejected (section 4).
    #[error(transparent)]
    Lobby(#[from] LobbyError),
    /// An in-round game command was rejected (section 8).
    #[error(transparent)]
    Input(#[from] InputError),
}
