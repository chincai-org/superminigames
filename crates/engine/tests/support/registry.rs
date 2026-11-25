//! A `GameRegistry` built from scripted fake factories (T2.4, TYP-2).

use superminigames_core::game::{Game, GameFactory};
use superminigames_core::ids::GameId;
use superminigames_engine::registry::{GameRegistry, RegisteredGame};

/// A `GameRegistry` of scripted fakes, built from factories in any order.
///
/// `GameRegistry::register` calls each factory once for a prototype instance
/// and keeps the games sorted by `GameId` (TYP-2), so the order the factories
/// are passed here does not affect registration or selection.
pub struct TestRegistry {
    registry: GameRegistry,
}

impl TestRegistry {
    /// Builds a registry from `factories`, registering each one (TYP-2).
    pub fn from_factories(factories: &[GameFactory]) -> Self {
        let mut registry = GameRegistry::new();
        for factory in factories {
            registry
                .register(*factory)
                .expect("TestRegistry: every factory must have a unique GameId");
        }
        Self { registry }
    }

    /// The wrapped registry.
    pub fn registry(&self) -> &GameRegistry {
        &self.registry
    }

    /// The registered game with `id`.
    pub fn game(&self, id: &str) -> &RegisteredGame {
        self.registry
            .get(&GameId(id.to_string()))
            .expect("TestRegistry: the game is registered")
    }

    /// A fresh, uninitialized instance of the game with `id` (RND-12).
    pub fn create(&self, id: &str) -> Box<dyn Game> {
        self.game(id).create()
    }
}
