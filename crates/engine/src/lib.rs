#![forbid(unsafe_code)]

pub mod actor;
pub mod error;
pub mod lobby;
pub mod presence;
pub mod registry;
pub mod replay;
pub mod report;
pub mod round;
pub mod selection;

pub use actor::{Command, EngineEvent, EngineHandle, EngineInit, EngineOptions, spawn_engine};
// TYP-1: the engine's phase type is the `logic` one, re-exported here.
pub use superminigames_logic::phase::Phase;
