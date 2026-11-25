#![forbid(unsafe_code)]
//! Wire types shared by the server and every client (SPEC section 14).
//!
//! The default encoding is MessagePack with named fields inside a versioned
//! envelope (PRO-1, PRO-2, PRO-3); JSON exists only behind the `json` feature for
//! debugging (PRO-1). Nothing here describes engine state, RNG state or a replay
//! log (PRO-4), and nothing here lets a client state a score, a place or an
//! outcome (PRO-5).

pub mod client_msg;
pub mod envelope;
pub mod error_code;
pub mod server_msg;
pub mod version;

pub use client_msg::{ClientMessage, LobbyConfig, LobbyVisibility, SettingsUpdate};
pub use envelope::{
    ClientEnvelope, Envelope, ProtocolError, ServerEnvelope, decode_client, decode_server,
    encode_client, encode_server,
};
pub use error_code::ErrorCode;
pub use server_msg::{
    LobbySnapshot, Phase, Placement, PlayerPoints, PlayerStatus, PublicLobby, QueueState,
    RankEntry, Seat, ServerMessage, SkipReason, Standing,
};
pub use version::{MAX_MESSAGE_BYTES, PROTOCOL_VERSION, RECONNECT_GRACE_SECS};

#[cfg(feature = "json")]
pub use envelope::{decode_client_json, encode_client_json};
