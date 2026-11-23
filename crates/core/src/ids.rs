//! Identifiers and small lobby types (TYP-1).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::ConfigError;

/// A player's identity, assigned by the server (TYP-3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PlayerId(pub Uuid);

/// A lobby's identity, assigned by the server (TYP-3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LobbyId(pub Uuid);

/// A match's identity, assigned by the server (TYP-3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MatchId(pub Uuid);

/// A game's stable identity. Ordering is byte order of the string (TYP-1).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct GameId(pub String);

impl std::fmt::Display for GameId {
    /// The game's own id, so `ConfigError` can name the game (LOB-12).
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A player as seen in a lobby (TYP-1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Player {
    pub id: PlayerId,
    pub name: String,
}

/// Exactly 6 characters from `A-Z` and `0-9` (TYP-1).
///
/// The only constructor validates, so a `JoinCode` always holds a well-formed
/// code and [`JoinCode::as_str`] is safe to compare.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinCode(String);

/// The join code alphabet (TYP-1).
const JOIN_CODE_LEN: usize = 6;

impl JoinCode {
    /// Validates and wraps a join code (TYP-1, LOB-12).
    pub fn new(code: impl Into<String>) -> Result<Self, ConfigError> {
        let code = code.into();
        let well_formed = code.len() == JOIN_CODE_LEN
            && code
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit());
        if well_formed {
            Ok(Self(code))
        } else {
            Err(ConfigError::InvalidJoinCode)
        }
    }

    /// The validated code.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Whether a lobby is listed publicly (TYP-1, PRO-15).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Visibility {
    Public,
    Private(JoinCode),
}
