//! Wire version and shared protocol constants (PRO-2, PRO-10, SCP-4).

/// The only accepted `Envelope::v` (PRO-2, SCP-4).
///
/// Bumping this is the wire-compatibility switch: a message is compatible when
/// it only adds optional fields (PRO-2).
pub const PROTOCOL_VERSION: u16 = 1;

/// Largest accepted frame in bytes, 64 KiB (PRO-10).
///
/// The server checks the frame length before decoding, so a hostile frame is
/// rejected as `MalformedMessage` without a full parse (SEC-10, SEC-17).
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// Reconnect grace in seconds, for the client's countdown display (UIX-7).
///
/// The authoritative value is the engine's `RECONNECT_GRACE`; the client only
/// shows a countdown from the moment it sees `PlayerStatus::Away`, so it needs
/// the number here without depending on `engine` (spec 1.1).
pub const RECONNECT_GRACE_SECS: u64 = 30;
