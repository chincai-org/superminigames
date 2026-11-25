//! The game registry and the per-round pool (TYP-2, PLL-1, section 6).
//!
//! Registering a game calls its factory once to keep a prototype that is never
//! initialized or ticked; the prototype answers `supports_players` when the pool
//! is built. A duplicate `GameId` is rejected and the games are kept sorted by
//! `GameId`, so nothing depends on insertion order (TYP-2, PLL-1, RNG-8).

use superminigames_core::config::GameFilterMode;
use superminigames_core::error::ConfigError;
use superminigames_core::game::{Game, GameFactory, ScoringMode};
use superminigames_core::ids::GameId;

use crate::selection::OwnedPick;

/// One registered game: cached metadata, the prototype used only for
/// `supports_players`, and the factory that makes fresh instances (TYP-2).
pub struct RegisteredGame {
    id: GameId,
    name: &'static str,
    scoring_mode: ScoringMode,
    prototype: Box<dyn Game>,
    factory: GameFactory,
}

impl RegisteredGame {
    /// The game's stable id (TYP-2).
    pub fn id(&self) -> &GameId {
        &self.id
    }

    /// The game's display name, taken from the prototype (TYP-2).
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// How the game's outcomes are interpreted (TYP-2, SCR-1).
    pub fn scoring_mode(&self) -> ScoringMode {
        self.scoring_mode
    }

    /// A fresh, uninitialized instance from the factory (RND-12).
    pub fn create(&self) -> Box<dyn Game> {
        (self.factory)()
    }

    /// Whether the prototype supports `participant_count` players (PLL-1).
    fn supports_players(&self, participant_count: usize) -> bool {
        self.prototype.supports_players(participant_count)
    }

    /// An owning pick for a slot started on a spawned task (T2.6, T-PRS-9).
    ///
    /// A [`RoundPick`](crate::selection::RoundPick) borrows the registry, which
    /// a `'static` task cannot hold, so a slot that must keep reading its inbox
    /// while `initialize` runs takes the factory and seed by value.
    pub fn owned_pick(&self, game_seed: u64) -> OwnedPick {
        OwnedPick {
            game_id: self.id.clone(),
            factory: self.factory,
            scoring_mode: self.scoring_mode,
            game_seed,
        }
    }
}

/// The immutable set of games the engine may select, sorted by `GameId`
/// (TYP-2, PLL-1).
#[derive(Default)]
pub struct GameRegistry {
    games: Vec<RegisteredGame>,
}

impl GameRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self { games: Vec::new() }
    }

    /// Register a game and cache its prototype's metadata (TYP-2).
    ///
    /// The factory is called once to obtain the prototype. Returns
    /// [`ConfigError::DuplicateGameId`] when `id` is already registered; the
    /// registry stays sorted by `GameId`.
    pub fn register(&mut self, factory: GameFactory) -> Result<(), ConfigError> {
        let prototype = factory();
        let id = prototype.id();
        if self.games.iter().any(|game| game.id == id) {
            return Err(ConfigError::DuplicateGameId(id));
        }
        let name = prototype.name();
        let scoring_mode = prototype.scoring_mode();
        self.games.push(RegisteredGame {
            id,
            name,
            scoring_mode,
            prototype,
            factory,
        });
        self.games.sort_by_key(|game| game.id.clone());
        Ok(())
    }

    /// The registered games, sorted ascending by `GameId` (TYP-2).
    pub fn games(&self) -> &[RegisteredGame] {
        &self.games
    }

    /// The registered game with `id`, if any.
    pub fn get(&self, id: &GameId) -> Option<&RegisteredGame> {
        self.games.iter().find(|game| game.id() == id)
    }

    /// The eligible games for a round, computed fresh (PLL-1).
    ///
    /// Applies `filter` and keeps games whose prototype supports
    /// `participant_count` players. Filtering a `GameId`-sorted registry
    /// preserves that ascending order, so the result is already sorted
    /// (PLL-1 step 4). An empty result means `Skipped(EmptyPool)`; T2.5 owns
    /// that mapping.
    pub fn pool(&self, filter: &GameFilterMode, participant_count: usize) -> Vec<&RegisteredGame> {
        self.games
            .iter()
            .filter(|game| match filter {
                GameFilterMode::AllowList(ids) => ids.contains(game.id()),
                GameFilterMode::BanList(ids) => !ids.contains(game.id()),
            })
            .filter(|game| game.supports_players(participant_count))
            .collect()
    }
}
