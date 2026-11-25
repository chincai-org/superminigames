//! The versioned envelope and the MessagePack codec (PRO-1, PRO-2, PRO-3, PRO-10).
//!
//! Decoding is the untrusted-input boundary (SEC-10), so it does three things in
//! a fixed order: bound the frame, check the version, then parse the body.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::client_msg::ClientMessage;
use crate::error_code::ErrorCode;
use crate::server_msg::ServerMessage;
use crate::version::{MAX_MESSAGE_BYTES, PROTOCOL_VERSION};

/// Every message travels wrapped in this (PRO-2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope<T> {
    /// Must equal [`PROTOCOL_VERSION`]; any other value is rejected (PRO-2).
    pub v: u16,
    /// The message itself.
    pub msg: T,
}

impl<T> Envelope<T> {
    /// Wraps a message with the current version (PRO-2).
    pub fn new(msg: T) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            msg,
        }
    }

    /// Whether this envelope speaks the version this build implements (PRO-2).
    pub fn is_supported_version(&self) -> bool {
        self.v == PROTOCOL_VERSION
    }
}

/// A frame decoded from a client (PRO-6).
pub type ClientEnvelope = Envelope<ClientMessage>;
/// A frame sent to a client (PRO-7).
pub type ServerEnvelope = Envelope<ServerMessage>;

/// Why a frame was rejected (PRO-1, PRO-2, PRO-10).
///
/// `thiserror` is not available here (spec 1.1), so the impls are written out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProtocolError {
    /// The frame is longer than [`MAX_MESSAGE_BYTES`] (PRO-10).
    TooLarge {
        /// Length of the rejected frame.
        len: usize,
    },
    /// `v` is not [`PROTOCOL_VERSION`]; the server replies `ProtocolMismatch` and closes (PRO-2).
    UnsupportedVersion {
        /// The version the peer sent.
        got: u16,
    },
    /// The bytes are not a valid MessagePack encoding of this message (PRO-1).
    Malformed(String),
    /// The message could not be encoded; a bug on our side, never peer input (PRO-1).
    Unencodable(String),
}

impl ProtocolError {
    /// The `ErrorCode` the server reports for this rejection (PRO-2, PRO-8, PRO-10).
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::UnsupportedVersion { .. } => ErrorCode::ProtocolMismatch,
            Self::TooLarge { .. } | Self::Malformed(_) | Self::Unencodable(_) => {
                ErrorCode::MalformedMessage
            }
        }
    }
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge { len } => write!(
                formatter,
                "frame of {len} bytes exceeds the {MAX_MESSAGE_BYTES} byte limit"
            ),
            Self::UnsupportedVersion { got } => write!(
                formatter,
                "protocol version {got} is not supported, this build speaks {PROTOCOL_VERSION}"
            ),
            Self::Malformed(reason) => write!(formatter, "malformed message: {reason}"),
            Self::Unencodable(reason) => {
                write!(formatter, "message could not be encoded: {reason}")
            }
        }
    }
}

impl std::error::Error for ProtocolError {}

/// Only the version field, so a bad version is reported before the body is parsed (PRO-2).
#[derive(Deserialize)]
struct VersionProbe {
    v: u16,
}

/// Encodes a client frame with named fields (PRO-3).
pub fn encode_client(envelope: &ClientEnvelope) -> Result<Vec<u8>, ProtocolError> {
    encode(envelope)
}

/// Encodes a server frame with named fields (PRO-3).
pub fn encode_server(envelope: &ServerEnvelope) -> Result<Vec<u8>, ProtocolError> {
    encode(envelope)
}

/// Decodes a client frame, bounding the size and checking the version first (PRO-2, PRO-10).
pub fn decode_client(frame: &[u8]) -> Result<ClientEnvelope, ProtocolError> {
    decode(frame)
}

/// Decodes a server frame, bounding the size and checking the version first (PRO-2, PRO-10).
pub fn decode_server(frame: &[u8]) -> Result<ServerEnvelope, ProtocolError> {
    decode(frame)
}

fn encode<T: Serialize>(envelope: &Envelope<T>) -> Result<Vec<u8>, ProtocolError> {
    rmp_serde::to_vec_named(envelope).map_err(|error| ProtocolError::Unencodable(error.to_string()))
}

fn decode<T: DeserializeOwned>(frame: &[u8]) -> Result<Envelope<T>, ProtocolError> {
    if frame.len() > MAX_MESSAGE_BYTES {
        return Err(ProtocolError::TooLarge { len: frame.len() });
    }
    let probe: VersionProbe = rmp_serde::from_slice(frame)
        .map_err(|error| ProtocolError::Malformed(error.to_string()))?;
    if probe.v != PROTOCOL_VERSION {
        return Err(ProtocolError::UnsupportedVersion { got: probe.v });
    }
    rmp_serde::from_slice(frame).map_err(|error| ProtocolError::Malformed(error.to_string()))
}

/// Encodes a client frame as JSON, for debugging only (PRO-1).
#[cfg(feature = "json")]
pub fn encode_client_json(envelope: &ClientEnvelope) -> Result<String, ProtocolError> {
    serde_json::to_string(envelope).map_err(|error| ProtocolError::Unencodable(error.to_string()))
}

/// Decodes a client frame from JSON, for debugging only (PRO-1).
#[cfg(feature = "json")]
pub fn decode_client_json(frame: &str) -> Result<ClientEnvelope, ProtocolError> {
    if frame.len() > MAX_MESSAGE_BYTES {
        return Err(ProtocolError::TooLarge { len: frame.len() });
    }
    serde_json::from_str(frame).map_err(|error| ProtocolError::Malformed(error.to_string()))
}
