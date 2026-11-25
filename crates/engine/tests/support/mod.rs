//! The engine test harness (T2.4): scripted fake games and helpers shared by
//! the M2 engine tests (T2.5, T2.6).
//!
//! A test binary includes this module tree with
//! `#[path = "support/mod.rs"] mod support;`. It uses only `superminigames-core`
//! types and the engine's public API, so it needs no feature flag and compiles
//! as a plain module tree.
//!
//! Everything here is deterministic: no wall clock, no OS randomness, no I/O
//! and no `HashMap`/`HashSet` (GAM-2, CI-2).

#![allow(dead_code)] // each M2 test uses only a subset of the harness.

mod fake_game;
mod registry;

// `dead_code`/`unused_imports` are allowed because each M2 test uses only a
// subset of the harness; the rest is for T2.5 and T2.6.
#[allow(unused_imports)]
pub use fake_game::{
    FakeGame, FakeGameBuilder, InitGate, InitReply, TickScript, broadcast, rejected, to_player,
};
#[allow(unused_imports)]
pub use registry::TestRegistry;

use superminigames_core::ids::PlayerId;

/// A deterministic player id, so scripts and ids are stable across runs.
///
/// The value is the low 128 bits of `n` in uuid's 32-hex "simple" form, which
/// its serde impl accepts. This keeps `Uuid::from_u128` semantics without the
/// uuid crate (the engine may not depend on it) and never uses `Uuid::new_v4`
/// (TYP-3, CI-2).
pub fn player(n: u64) -> PlayerId {
    serde_json::from_str(&format!("\"{n:032x}\"")).expect("a 32-hex uuid literal")
}
