//! Game conformance suite and fake games (GAM-11, T-GAM-1 to T-GAM-5, T-GAM-7).
//!
//! Enabled by the `test-support` feature so every game crate can run the same
//! suite from its own tests:
//!
//! ```ignore
//! // tests/conformance.rs in a game crate
//! #[test]
//! fn t_gam_1_same_seed_same_run() {
//!     superminigames_core::test_support::conformance::t_gam_1_same_seed_same_run(
//!         my_crate::factory,
//!         &players,
//!         0,
//!         &superminigames_core::test_support::conformance::generic_script(&players),
//!     );
//! }
//! ```
//!
//! `core`'s own tests run the suite against [`fake::FakeGame`], so the suite
//! itself is covered before the real games exist (T1.7, CI-7).

pub mod conformance;
pub mod fake;

#[cfg(test)]
mod tests;
