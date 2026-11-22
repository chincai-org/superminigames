#![forbid(unsafe_code)]

pub mod config;
pub mod error;
pub mod game;
pub mod ids;
pub mod outcome;
pub mod rng;
pub mod scoring;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
