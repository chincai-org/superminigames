//! Deterministic selection, the game seed and the first mover (RNG-3 to RNG-5,
//! RNG-10, RNG-11, section 6).
//!
//! Every draw goes through the shared `superminigames-core` RNG helpers, so the
//! pick for a round depends only on `(seed, round, pool)`: retries reuse it and
//! never touch the RNG (RNG-6, RNG-7).

use rand_core::RngCore;
use superminigames_core::game::{GameFactory, ScoringMode};
use superminigames_core::ids::GameId;
use superminigames_core::rng::{chacha_from_seed, uniform_index};

use crate::registry::RegisteredGame;

/// The opening bit: `chacha_from_seed(seed, 1 << 63).next_u64() & 1` (RNG-11).
///
/// It decides the first mover of the first game of the match.
pub fn opening_bit(seed: u64) -> u8 {
    (chacha_from_seed(seed, 1 << 63).next_u64() & 1) as u8
}

/// The seat of the first mover for the game that is about to start (RNG-11).
///
/// `games_started` is the number of rounds that reached `InRound`; skipped
/// rounds do not change the alternation, so the role alternates every game that
/// starts.
pub fn first_mover_seat(opening: u8, games_started: usize) -> usize {
    (usize::from(opening) + games_started) & 1
}

/// The seed passed to `Game::initialize` for round `round` (RNG-10).
///
/// It is the first `next_u64()` of `chacha_from_seed(seed, u64::MAX - round)`.
/// This stream never collides with a selection stream because `rounds <= 100`.
pub fn game_seed(seed: u64, round: u64) -> u64 {
    chacha_from_seed(seed, u64::MAX - round).next_u64()
}

/// The game selected for one round and the seed it is initialized with (RNG-3,
/// RNG-10).
pub struct RoundPick<'a> {
    /// The selected game, borrowed from the round's pool.
    pub game: &'a RegisteredGame,
    /// The game seed for this round; identical on every retry (RNG-6).
    pub game_seed: u64,
}

/// A `RoundPick` taken by value for a slot whose start is spawned (T2.6,
/// T-PRS-9): the factory and seed move to the task, so it borrows nothing from
/// the registry and the actor can keep reading its inbox while `initialize`
/// runs.
pub struct OwnedPick {
    /// The selected game's id, cached at registration (TYP-2).
    pub game_id: GameId,
    /// The selected game's factory, a plain function pointer (TYP-2).
    pub(crate) factory: GameFactory,
    /// The selected game's scoring mode, cached at registration (TYP-2).
    pub(crate) scoring_mode: ScoringMode,
    /// The game seed for this round; identical on every retry (RNG-6).
    pub game_seed: u64,
}

/// Pick one game from `pool` for `round` (RNG-3, RNG-4, RNG-10).
///
/// Exactly one `uniform_index` draw is made on the round's own stream, so the
/// result depends only on `(seed, round, pool)`. Returns `None` when `pool` is
/// empty; T2.5 maps that to `Skipped(EmptyPool)`.
pub fn pick<'a>(pool: &[&'a RegisteredGame], seed: u64, round: u64) -> Option<RoundPick<'a>> {
    if pool.is_empty() {
        return None;
    }
    let mut rng = chacha_from_seed(seed, round);
    let index = uniform_index(&mut rng, pool.len());
    Some(RoundPick {
        game: pool[index],
        game_seed: game_seed(seed, round),
    })
}
