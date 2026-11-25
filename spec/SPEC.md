# SuperMiniGames Specification (Match Rules and Rust Implementation)

Version 0.1 | Language: Rust (edition 2024) | Audience: implementer (human or AI)

This spec owns requirements, constants, message names and semantics, and game rules. `spec/PLAN.md` owns architecture, database schema, HTTP and WebSocket field shapes, tech stack, folder layout and milestones. If they conflict, this spec wins.

This document merges the match rules spec and the Rust implementation spec. Match rules are in sections 4-10. Architecture is in sections 1, 3, 11, 13-15. The verification and change policy is in section 19. Latency, performance, security and the initial games are in sections 20-22. Joining and the client UI are in section 23.

## 0. Conventions

- **MUST**, **MUST NOT**, **SHOULD**, **MAY** follow RFC 2119.
- Every requirement has an ID such as `RND-4`. Cite IDs in code comments and test names.
- Normative: sections 1-15, 17 and 19-23. Informative: section 16 (worked example) and section 18 (decision log). If an informative section conflicts with a numbered requirement, the requirement wins.
- Code blocks define names, signatures and semantics. You MAY add fields and helper types. You MUST NOT remove or rename the fields, variants and methods shown. Blocks marked "illustrative" are examples, not requirements.
- Anything this spec leaves open: you MAY choose, but the choice MUST be deterministic where determinism matters (sections 6 and 13) and MUST be recorded in a code comment starting with `SPEC-GAP:`.
- **Confirmed** means decided by the product owner. **Default** means chosen by this spec; the product owner MAY change it (see section 18).

## 1. Scope and workspace

- SCP-1: The deliverable is a Rust workspace: a deterministic match engine, a network server, wire-protocol types shared by server and clients, a client integration layer, and one crate per mini-game.
- SCP-2: Out of scope: UI design and the choice of UI framework for web/desktop/mobile, database engine and schema, OAuth or guest login, the choice of email provider (SMTP is assumed), multi-region deployment, and deployment tooling. The rules of the five initial games are in section 22; later games are specified separately.
- SCP-3: Design goals: keep game logic deterministic where possible; separate game rules from network code; make rounds restartable and testable; keep UI independent of engine logic; let web, desktop and mobile clients reuse one protocol.
- SCP-4: Constants: `MIN_PLAYERS = 2`, `MAX_PLAYERS = 2` (Confirmed: 2 players only for now; the scoring functions stay defined for 2 to 4, SCR-12), `START_ATTEMPTS = 4`, `POINTS_SCALE = 12`, `INBOX_CAPACITY = 256`, `PROTOCOL_VERSION = 1`, `LAG_CAP_MS = 1000`, `LAG_TOTAL_CAP_MS = 10_000`, `LOBBY_IDLE_TIMEOUT = 600` seconds, `RECONNECT_GRACE = 30` seconds.

### 1.1 Crates

| Crate | Responsibility | MAY depend on | MUST NOT depend on |
|---|---|---|---|
| `superminigames-logic` | Pure, formally verifiable decision logic: place and points computation, ranking, removal-streak update, phase transitions, missing-outcome rule. Primitive types only (section 19). | nothing (plus `vstd` and `builtin` when Verus is enabled) | `serde`, `tokio`, any I/O crate, `core`, wall-clock time, randomness |
| `superminigames-core` | Ids, config types, the `Game` trait and game-facing types, outcomes, thin scoring adapters over `logic`, shared RNG helpers, shared errors. | `logic`, `serde`, `serde_bytes`, `thiserror`, `uuid`, `rand_core`, `rand_chacha` | `tokio`, any I/O crate, wall-clock time, OS randomness |
| `superminigames-protocol` | Wire types: `Envelope`, `ClientMessage`, `ServerMessage`, `ErrorCode`, version constant. | `core`, `serde`, `serde_bytes`, `rmp-serde`, `uuid`, `serde_json` (only behind an optional `json` debug feature) | `tokio`, `axum`, `engine`, `server` |
| `superminigames-engine` | Lobby and match state machine, game selection, round runner, timers, presence, score aggregation, replay recording. | `core`, `logic`, `tokio` (sync, time, macros, rt), `rand_core`, `rand_chacha`, `thiserror`, `tracing`, `serde` | `axum`, `sqlx`, `protocol`, `chrono`, wall-clock time, OS randomness |
| `superminigames-server` | Transport, session and auth boundary, hosting one engine actor per lobby, join codes, persistence, metrics. | `engine`, `protocol`, `core`, `axum`, `rustls`, `tokio`, `sqlx`, `tracing`, `uuid`, `chrono`, `rand`, `argon2`, `sha2`, `lettre` | `client` |
| `superminigames-client` | Client-side integration layer: state mirror, command helpers, bindings used by web, desktop and mobile wrappers. | `protocol`, `core`, game crates (for legal-move hints, UIX-9) | `engine`, `server`, `tokio` runtime assumptions that break WASM |
| `superminigames-games/<name>` | One crate per mini-game, implementing `Game` for exactly that game. | `core`, `rand_chacha`, `rand_core` | `engine`, `server`, `protocol`, `tokio`, other game crates |

- WSP-1: Layers MUST stay separate. Game rules live only in game crates. Match orchestration lives only in `engine`. Networking lives only in `server`. UI lives only in client wrappers. Persistence lives only in the server's storage module.
- WSP-2: Dependency direction is: `core` depends on `logic`; games and `protocol` depend on `core`; `engine` depends on `core` and `logic`; `server` depends on `engine`, `protocol`, `core`; `client` depends on `protocol` and `core`. No cycles.
- WSP-3: `core` and `protocol` SHOULD compile for `wasm32-unknown-unknown` so browser clients reuse them.
- WSP-4: Each game crate exposes `pub fn create() -> Box<dyn Game>` matching `GameFactory` (section 3). The server binary registers every game with the `GameRegistry` at startup.
- WSP-5: `core` MUST NOT contain game rules. It contains only types, the `Game` trait, thin scoring adapters over `logic`, and the shared RNG helpers.
- WSP-6: CI MUST enforce the dependency rules in the table (for example with `cargo-deny` bans or a `cargo tree` check). See T-WSP-1.
- WSP-7: The browser frontend, desktop wrapper and mobile wrapper are separate projects that consume `protocol` and `client`; they are out of scope here.
- WSP-8: Strongly typed ids MUST be newtypes, never raw strings: `PlayerId`, `LobbyId`, `MatchId`, `GameId`.
- WSP-9: **Game-owned rules.** No rules code is shared between game crates, and game crates do not depend on each other. `core` has no rules, no draw logic and no clock logic. Each game implements its own legality, termination, draw, repetition and clock functions. The `Game` trait is the only unified interface, and what the match consumes from a game is its result (`result()` mapped to places, SCR-3).

## 2. Glossary

| Term | Definition |
|---|---|
| Player | A person identified by a `PlayerId`. |
| Host | The player who created the lobby. The host is also a player and holds a seat. |
| Seat order | Order in which players joined the lobby (host first). Used wherever a deterministic player order is needed. |
| Lobby | The pre-match room: seats, ready flags, settings, visibility. |
| Match | One run of N round slots after the lobby starts. |
| Round slot | One index `0..rounds-1`. Every slot is processed exactly once, even if the round is skipped. |
| Registry | The set of all games known to the engine, each registered through a `GameFactory`. |
| Pool | The games eligible for one specific round (after filtering and player-count support). |
| Connected | The engine's flag for a player, changed only by `Command::Connection` events from the server. |
| Present | A player's `connected` flag was `true` at a round's start check. |
| InMatch | A player whose `Standing` is not `Removed`. Round-disqualified players are still InMatch. |
| Participant | An InMatch player who is Present at a round's start check. Only participants play that round. |
| Absent streak | Number of consecutive round-start checks at which an InMatch player was not Present. Resets to 0 when Present. |
| Removed | Permanent state reached when the absent streak reaches K. Removed players never play again. |
| K | `max_absent_round_starts` from `MatchConfig`. Default 3. |
| Round disqualification | A participant has no determined outcome in a round because the timer expired or they left mid-round. Applies to that round only. |
| In-game elimination | A player eliminated by the game's own rules (e.g. a player checkmated in a future 4-player chess game). NOT a disqualification; the game still produces a normal outcome for them. |
| Tick | One call to `Game::tick` with one `GameInput`. |
| Placement (place) | Positive integer rank in a round. Ties share the same place and following places are skipped (1, 1, 3, 4). |
| Points | Integer round score derived from the place. 12 points for an outright 1st place; tied players split the points of the places they occupy. |
| Skipped round | A round slot where no points are awarded to anyone. |
| Draw | A game result in which both players share 1st place, if that game's own rules produce one. Each game decides its own draw conditions, and some may have none. Draws split the points (6 each at scale 12). |
| Time model | Per game: `PlayerClocks` (the game keeps per-player clocks) or `RoundDeadlineOnly` (only the round deadline limits play). |
| Hard cap | The round deadline. It applies to every game whatever its time model. |
| Lag credit | Milliseconds of network delay credited back to a mover, computed by the server from measured round-trip time and capped. |
| Reconnect grace | 30 s after a mid-round disconnect during which the player may return and keep playing the same round; clocks keep running (PRS-6). |
| Actor | The single tokio task that owns all state of one lobby and its match. |
| Replay log | Recorded game seed, players and ordered inputs of one round, enough to reproduce it exactly. |

## 3. Core types and contracts

- TYP-1: The types below are normative in names and semantics (see section 0). Types derive `Serialize` and `Deserialize` where they cross a crate or network boundary; engine-only state does not (PRO-4).

```rust
// ===================== superminigames-core =====================
use std::time::Duration;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------- identifiers ----------
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PlayerId(pub Uuid);
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LobbyId(pub Uuid);
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MatchId(pub Uuid);
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct GameId(pub String); // ordering = byte order of the string

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Player { pub id: PlayerId, pub name: String }

/// Exactly 6 characters from A-Z and 0-9. Constructor validates.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinCode(String);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Visibility { Public, Private(JoinCode) }

// ---------- configuration ----------
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum GameFilterMode {
    AllowList(Vec<GameId>), // only these games are eligible
    BanList(Vec<GameId>),   // these games are excluded
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MatchConfig {
    pub rounds: u32,                  // 1..=100
    pub round_time_limit_secs: u64,   // 1..=3600
    pub seed: u64,                    // set at StartMatch by the server (SRV-10); tests use 0
    pub max_absent_round_starts: u32, // K; default 3; 1..=10
    pub filter: GameFilterMode,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SettingsUpdate {
    pub rounds: Option<u32>,
    pub round_time_limit_secs: Option<u64>,
    pub max_absent_round_starts: Option<u32>,
    pub filter: Option<GameFilterMode>,
    pub visibility: Option<Visibility>,
}

// ---------- game contract ----------
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScoringMode {
    Placement,      // game reports final places
    HigherIsBetter, // game reports raw scores; higher ranks better
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeModel {
    PlayerClocks,      // the game keeps per-player clocks (chess, doushouqi)
    RoundDeadlineOnly, // only the round deadline limits play
}

/// Opaque action payload in the game's own compact binary encoding. The game validates it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GameAction(#[serde(with = "serde_bytes")] pub Vec<u8>);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum GameInput {
    Player { player: PlayerId, action: GameAction, lag_credit_ms: u32 },
    PlayerLeft(PlayerId), // sent by the engine when a participant leaves mid-round
    TimeAdvanced,         // sent by the engine when the game's `next_wakeup_ms` is reached
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// Opaque event payload in the game's own compact binary encoding.
pub struct Payload(#[serde(with = "serde_bytes")] pub Vec<u8>);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum GameEvent {
    Broadcast(Payload),                       // visible to all participants
    ToPlayer { player: PlayerId, payload: Payload }, // private to one player
    InputRejected { player: PlayerId, reason: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DisqualificationReason { TimerExpired, LeftRound }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlayerOutcome {
    Placed(u8),                       // final place from the game; ties repeat the value
    Scored(i64),                      // raw score (HigherIsBetter games only)
    NoResult(DisqualificationReason), // nothing determined for this player
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GameResult {
    pub outcomes: Vec<(PlayerId, PlayerOutcome)>,
}

#[derive(Debug, thiserror::Error)]
pub enum GameError {
    #[error("game failed to initialize: {0}")] InitFailed(String), // engine MAY retry
    #[error("game failed during play: {0}")]   Fatal(String),      // no retry
}

pub trait Game: Send {
    fn id(&self) -> GameId;
    fn name(&self) -> &'static str;
    fn scoring_mode(&self) -> ScoringMode;
    fn time_model(&self) -> TimeModel;
    fn supports_players(&self, count: usize) -> bool;
    /// Set up a fresh round. MUST fully reset all state.
    fn initialize(&mut self, seed: u64, players: &[PlayerId], time_limit_ms: u64) -> Result<(), GameError>;
    /// `at_ms` = milliseconds since round start, measured by the engine.
    fn tick(&mut self, at_ms: u64, input: GameInput) -> Result<Vec<GameEvent>, GameError>;
    /// Next moment (ms since round start) the game needs a tick even if no input arrives, e.g. a clock running out.
    fn next_wakeup_ms(&self) -> Option<u64>;
    fn is_finished(&self) -> bool;
    /// Outcomes the game considers determined (or is willing to rank) right now.
    fn result(&self) -> GameResult;
}

/// Each game crate exports a function of this type.
pub type GameFactory = fn() -> Box<dyn Game>;

// ---------- scoring (thin adapters over superminigames-logic, sections 9 and 19) ----------
pub const POINTS_SCALE: u32 = 12;
pub fn compute_places(outcomes: &[PlayerOutcome]) -> Vec<(usize, usize)>; // (place, tie group size)
pub fn points_for_place(n: usize, place: usize, group: usize) -> u32;

// ---------- shared RNG helpers (section 6) ----------
// pub fn chacha_from_seed(seed: u64, stream: u64) -> rand_chacha::ChaCha8Rng;
// pub fn uniform_index(rng: &mut rand_chacha::ChaCha8Rng, n: usize) -> usize;
```

```rust
// ===================== superminigames-engine =====================
pub use superminigames_logic::Phase; // Lobby, Starting, InRound, RoundFinished, MatchFinished, Cancelled

pub enum Standing { InMatch, Removed } // Removed is permanent

pub struct PlayerState {
    pub player: Player,
    pub ready: bool,         // meaningful in the lobby only
    pub connected: bool,     // changed only by Command::Connection
    pub standing: Standing,
    pub absent_streak: u32,
    pub total_points: u32,
}

pub struct LobbySnapshot {
    pub lobby_id: LobbyId,
    pub host: PlayerId,
    pub seats: Vec<(Player, bool)>, // (player, ready)
    pub config: MatchConfig,
    pub private: bool,              // the join code itself is never included
    pub settings_locked: bool,
}

pub enum SkipReason {
    NotEnoughPlayers, // fewer than 2 participants
    EmptyPool,        // no eligible game
    StartFailed,      // all START_ATTEMPTS failed
    NoResults,        // every participant ended with NoResult
    InvalidReport,    // the game result failed validation
    GameAborted,      // game returned GameError::Fatal or panicked
}

pub enum RoundStatus { Scored, Skipped(SkipReason) }

pub struct ReplayLog {
    pub game: GameId,
    pub game_seed: u64,
    pub players: Vec<PlayerId>,
    pub time_limit_ms: u64,
    pub inputs: Vec<(u64, GameInput)>, // (at_ms, input) in the exact order ticked
}

pub struct RoundRecord {
    pub round_index: u32,
    pub game: Option<GameId>,
    pub participants: Vec<PlayerId>,
    pub attempts: u8,
    pub status: RoundStatus,
    pub placements: Vec<(PlayerId, u8, u32)>, // (player, place, points); empty if Skipped
    pub round_disqualified: Vec<(PlayerId, DisqualificationReason)>,
    pub removed_this_round: Vec<PlayerId>,
    pub replay: Option<ReplayLog>,            // only when EngineOptions.record_replays
}

pub struct RankEntry {
    pub player: PlayerId,
    pub rank: u32, // competition ranking: ties share a rank
    pub total_points: u32,
    pub standing: Standing,
}

pub struct MatchReport {
    pub match_id: MatchId,
    pub lobby_id: LobbyId,
    pub config: MatchConfig,
    pub rounds: Vec<RoundRecord>,
    pub final_ranking: Vec<RankEntry>,
    pub ended_early: bool, // true if the end condition fired before all slots were processed
}

pub enum PlayerStatus { Present, Away, Removed }

pub enum Command {
    Join { player: Player, code: Option<JoinCode> },
    Leave { player: PlayerId },
    SetReady { player: PlayerId, ready: bool },
    UpdateSettings { player: PlayerId, update: SettingsUpdate },
    Cancel { player: PlayerId },
    StartMatch { player: PlayerId, seed: u64 }, // seed drawn by the server (SRV-10)
    Connection { player: PlayerId, connected: bool }, // from the server's transport layer
    Input { player: PlayerId, action: GameAction, lag_credit_ms: u32, received_at: tokio::time::Instant },   // in-round game input
}

pub enum EngineEvent {
    LobbyChanged(LobbySnapshot),
    PhaseChanged(Phase),
    RoundStarting { round_index: u32, game: GameId, participants: Vec<PlayerId>, time_limit_secs: u64 },
    Game { round_index: u32, event: GameEvent },
    PlayerStatus { player: PlayerId, status: PlayerStatus },
    RoundFinished(RoundRecord),
    MatchFinished(MatchReport),
}

pub struct EngineOptions { pub record_replays: bool, pub inbox_capacity: usize }

pub struct EngineInit {
    pub lobby_id: LobbyId,
    pub match_id: MatchId, // assigned by the server; the engine never generates ids
    pub host: Player,
    pub config: MatchConfig,
    pub visibility: Visibility,
    pub registry: GameRegistry,
    pub options: EngineOptions,
}

// ---------- errors (use thiserror) ----------
pub enum ConfigError {
    RoundsOutOfRange, TimeLimitOutOfRange, AbsentLimitOutOfRange,
    EmptyAllowList, DuplicateGameInList, UnknownGame(GameId),
    NoEligibleGames, DuplicateGameId(GameId), InvalidJoinCode,
}

pub enum LobbyError {
    WrongState, NotHost, LobbyFull, InvalidJoinCode, SettingsLocked,
    NotAllReady, NotEnoughPlayers, UnknownPlayer, Config(ConfigError),
}

pub enum InputError { NotInRound, NotParticipant, PlayerLeftRound }

pub enum EngineError { Lobby(LobbyError), Input(InputError) }
```

- TYP-2: `GameRegistry::register(factory: GameFactory)` MUST call the factory once to obtain a prototype instance (never initialized or ticked), cache its `id`, `name` and `scoring_mode`, and keep the factory. It MUST reject a duplicate `GameId` with `ConfigError::DuplicateGameId`. The registry is sorted by `GameId` and immutable once an engine is running.
- TYP-3: The engine MUST NOT generate ids or read wall-clock time. It uses `tokio::time` only (ARC-4). `LobbyId`, `MatchId`, the seed and timestamps are supplied or added by the server.

## 4. Lobby

- LOB-1: The player who creates the lobby becomes the host and holds seat 0. The host is a player and MUST press Ready like everyone else.
- LOB-2: A lobby holds at most `MAX_PLAYERS` players, which is 2 for now (Confirmed; see SCR-12). A join attempt when full MUST fail with `LobbyFull`.
- LOB-3: Visibility is `Public` or `Private(JoinCode)`. A join to a private lobby MUST fail with `InvalidJoinCode` if the code is missing or not an exact (case-sensitive) match. Code generation and rate limiting belong to the server (SRV-4).
- LOB-4: Lobby commands (`Join`, `Leave`, `SetReady`, `UpdateSettings`, `Cancel`, `StartMatch`) are accepted only in `Phase::Lobby`; otherwise they MUST fail with `LobbyError::WrongState`. Exceptions: `Leave` and `Connection` are also handled during a match (section 8).
- LOB-5: Any seated player MAY set Ready or unready any number of times while the phase is `Lobby`.
- LOB-6: **Settings lock.** `UpdateSettings` (rounds, time limit, K, filter, visibility, join code) MUST be accepted only from the host and only while no seated player is Ready. If any player is Ready, it MUST fail with `SettingsLocked`. When all players have unreadied, settings unlock again. The match seed is not a setting: it is supplied by the server at `StartMatch` (LOB-9, SRV-10).
- LOB-7: At `StartMatch` the settings lock permanently (phase leaves `Lobby`).
- LOB-8: Allow-list and ban-list modes are mutually exclusive. The `GameFilterMode` enum enforces this; the engine MUST also validate the filter contents (LOB-12).
- LOB-9: `StartMatch` is accepted only from the host, with at least `MIN_PLAYERS` seated and every seated player Ready. Otherwise fail with `NotHost`, `NotEnoughPlayers` or `NotAllReady`. On success the engine stores the server-supplied `seed` in `config.seed`, the seat list is frozen and the phase becomes `Starting` for slot 0.
- LOB-10: `Cancel` is accepted only from the host and only in `Phase::Lobby`. The phase becomes `Cancelled`. After the match starts, `Cancel` MUST fail with `WrongState`.
- LOB-11: If the host leaves while in `Lobby`, the lobby becomes `Cancelled`. If a non-host leaves, their seat is freed and their Ready flag is discarded.
- LOB-12: Config validation (at `UpdateSettings` and again at `StartMatch`):
  - `rounds` in 1..=100, `round_time_limit_secs` in 1..=3600, `max_absent_round_starts` in 1..=10.
  - Filter lists contain no duplicate ids and only ids present in the registry (`DuplicateGameInList`, `UnknownGame`).
  - An allow list MUST NOT be empty (`EmptyAllowList`).
  - After filtering the registry (ignoring player counts), at least one game MUST remain (`NoEligibleGames`).
- LOB-13: Commands MUST be processed strictly one at a time, in arrival order, by the actor (ARC-2). When two commands race (e.g. Ready and UpdateSettings), the one processed first wins and the later one gets its normal error.
- LOB-14: No new players may join after `StartMatch`. Returning after leaving is handled only through presence (section 8), not through `Join`.
- LOB-15: In `Phase::Lobby`, `Connection { connected: false }` for a seated player MUST be treated exactly like `Leave` for that player. A `Connection { connected: true }` for an unseated player is ignored.
- LOB-16: **Idle lobbies.** A lobby in `Phase::Lobby` that receives no command for `LOBBY_IDLE_TIMEOUT` MUST be cancelled by the actor (phase `Cancelled`), so abandoned lobbies do not leak tasks.

## 5. Match and round flow

- MTH-1: A match has `config.rounds` round slots, indexed `0..rounds-1`. Slots are processed in order, each exactly once. A skipped round still consumes its slot.
- MTH-2: Round procedure. For each slot, the engine MUST perform these steps in this order:

- RND-1: Phase `Starting`. Presence check and removal (PRS-2, PRS-3).
- RND-2: If the number of InMatch players is 1 or fewer, go to `MatchFinished` now (before running this slot). Set `ended_early = true`.
- RND-3: Participants = InMatch players that are Present, in seat order. If fewer than 2, record `Skipped(NotEnoughPlayers)` and go to `RoundFinished`.
- RND-4: Build the pool (section 6). If empty, record `Skipped(EmptyPool)` and go to `RoundFinished`.
- RND-5: Select one game and derive the game seed (section 6).
- RND-6: Attempts (section 10). Each attempt creates a fresh instance from the factory and calls `initialize`. On success go to `InRound`. After all attempts fail, record `Skipped(StartFailed)` and go to `RoundFinished`.
- RND-7: Phase `InRound`. Run the in-round loop (section 7).
- RND-8: When the round ends (TMR-3, TMR-4), collect the `GameResult` by calling `result()` exactly once.

- RND-9: **Result validation.** A `GameResult` is valid only if all hold: every entry names a participant; no player appears twice; each `Placed(v)` has `1 <= v <= n` (n = participant count); `Placed` appears only if `scoring_mode() == Placement`; `Scored` appears only if `scoring_mode() == HigherIsBetter`; and if `is_finished()` was true when the result was collected, every participant appears. Participants missing from a valid result MUST be filled in by the engine: `NoResult(LeftRound)` if the player left mid-round (PRS-6), otherwise `NoResult(TimerExpired)`. An invalid result MUST produce `Skipped(InvalidReport)`.
- RND-10: Validate the result (RND-9), score the round (section 9) and add the points to the totals. Then enter phase `RoundFinished`, append the `RoundRecord` and emit events. If slots remain, go to `Starting` for the next slot; otherwise go to `MatchFinished`.
- RND-11: Every participant MUST be passed to `initialize` in first-mover order: `players[0]` moves first (RNG-11). Records and tie ordering use seat order.
- RND-12: The board game MUST be fully reset every round: every attempt uses a fresh instance from the factory, and the engine MUST NOT carry any game state between rounds or attempts.
- RND-13: Disconnects between the presence snapshot and the start of play: for every participant whose `connected` flag is `false` right after a successful `initialize`, the engine starts their reconnect grace at `at_ms = 0` (PRS-6). If every participant's grace has expired, the engine MUST end the round immediately (treat as the deadline, TMR-4).
- MTH-3: When the phase becomes `MatchFinished` (after the last slot, or by RND-2), the engine MUST produce a `MatchReport` with the final ranking (FIN-1) and emit `EngineEvent::MatchFinished`.

## 6. Game selection and randomness

- PLL-1: The pool for a round MUST be computed fresh each round:
  1. Start from the registry.
  2. Apply the filter: `AllowList(ids)` keeps only games in `ids`; `BanList(ids)` removes games in `ids`.
  3. Keep only games where the prototype's `supports_players(participants.len())` is `true`.
  4. Sort ascending by `GameId`.
- PLL-2: Selection is uniform: every game in the pool has probability `1 / pool.len()`.
- RNG-1: The RNG MUST be `rand_chacha::ChaCha8Rng`. No other RNG algorithm is permitted for selection or for games.
- RNG-2: The key is built from a seed: 32 bytes, with the seed's 8 little-endian bytes in positions 0..8 and zeros elsewhere. Seed 0 means an all-zero key. Use `from_seed`, not `seed_from_u64`.
- RNG-3: Each round uses its own stream: the selection RNG for round k is the keyed RNG with `set_stream(k as u64)` before the first draw. The pick for round k therefore depends only on (seed, k, pool).
- RNG-4: Selection MUST use exactly one `uniform_index` call per round that reaches selection. Skipped rounds (`NotEnoughPlayers`, `EmptyPool`) make no draw.
- RNG-5: Index sampling MUST use this rejection-sampling algorithm (do NOT use `gen_range` or any `Distribution`, whose output may change between crate versions). The helpers live in `superminigames-core` as the single shared implementation:

```rust
use rand_chacha::ChaCha8Rng;
use rand_core::{RngCore, SeedableRng};

pub fn chacha_from_seed(seed: u64, stream: u64) -> ChaCha8Rng {
    let mut key = [0u8; 32];
    key[..8].copy_from_slice(&seed.to_le_bytes());
    let mut rng = ChaCha8Rng::from_seed(key);
    rng.set_stream(stream);
    rng
}

/// Unbiased index in 0..n via rejection sampling.
pub fn uniform_index(rng: &mut ChaCha8Rng, n: usize) -> usize {
    assert!(n > 0);
    let n = n as u64;
    let limit = u64::MAX - (u64::MAX % n); // largest multiple of n that fits
    loop {
        let x = rng.next_u64();
        if x < limit {
            return (x % n) as usize;
        }
    }
}

// pick = &pool[uniform_index(&mut chacha_from_seed(seed, round_index), pool.len())]
```

- RNG-6: Retries (section 10) MUST reuse the same selected game and the same game seed, and MUST NOT touch the RNG.
- RNG-7: The engine, `core` and game crates MUST NOT use any other randomness (no `thread_rng`, no OS RNG, no random hashing state, no `Uuid::new_v4`). Only the server may use non-deterministic randomness (SRV-4).
- RNG-8: Engine logic MUST NOT depend on `HashMap` or `HashSet` iteration order. Use `BTreeMap`, `BTreeSet` or sorted `Vec`.
- RNG-9: The golden tests (T-RNG-1, T-RNG-6) MUST exist and MUST pass in CI.
- RNG-10: **Game seed.** For round k, `game_seed` is the first `next_u64()` of `chacha_from_seed(config.seed, u64::MAX - k as u64)`. It is computed once per round, after selection, and passed to `Game::initialize` on every attempt. The selection stream and the game-seed stream never collide because `rounds <= 100`.
- RNG-11: **First mover.** At `StartMatch` the engine computes `opening = chacha_from_seed(config.seed, 1 << 63).next_u64() & 1`. It keeps `games_started`, the number of rounds that reached `InRound`. The first mover of a game is the participant at seat `(opening + games_started) & 1`, so the first game's first mover is random and the role then alternates every game that starts. Skipped rounds do not change the alternation. (For more than 2 players the rule is defined together with the first multi-player game.)

## 7. Timers and the in-round loop

- TMR-1: Each round has one hard deadline. When the phase becomes `InRound` (immediately after a successful `initialize`), the engine captures `round_start = tokio::time::Instant::now()` and sets `deadline = round_start + time_limit`. `at_ms` is the number of whole milliseconds between `round_start` and the moment the event was received: for `Input`, the server-supplied `received_at` (never earlier than `round_start`); for every other event, the moment the engine dequeues it. The engine MUST keep `at_ms` non-decreasing across ticks (`at_ms = max(at_ms, previous at_ms)`), so inbox queuing delay is not charged to a mover.
- TMR-2: While `InRound`, the actor MUST wait on these at once: its inbox, `sleep_until(deadline)`, the earliest reconnect-grace expiry of an away participant, and (if the game returns `Some(t)` from `next_wakeup_ms`) `sleep_until(round_start + t ms)`. For each event:
  - `Input` from a participant who has not left this round: call `tick(at_ms, GameInput::Player { .. })` and forward the returned events as `EngineEvent::Game`.
  - `Input` from a non-participant: reply `InputError::NotParticipant`. From a participant who left this round: reply `InputError::PlayerLeftRound`.
  - `Connection { connected: false }` for a participant: mark them away and start their grace (PRS-6). `Connection { connected: true }` within the grace: mark them present again.
  - A participant's grace expires: mark them left this round and call `tick(at_ms, GameInput::PlayerLeft(id))` (PRS-6).
  - The game's wake-up time is reached: call `tick(at_ms, GameInput::TimeAdvanced)`. After every tick, re-read `next_wakeup_ms`.
  - Any lobby command: reply `LobbyError::WrongState`.
- TMR-3: After every successful tick, if `is_finished()` is `true`, the round ends immediately and the engine calls `result()` (no timer expiry).
- TMR-4: At the deadline the engine MUST immediately stop feeding inputs, call `result()` exactly once, and treat every participant missing from it as round-disqualified (RND-9). Inputs arriving afterwards MUST be rejected with `InputError::NotInRound`. The deadline is a hard cap for every game, whatever its time model.
- TMR-5: Timer expiry MUST NOT change a player's `Standing` or absent streak.
- TMR-6: Ticks run inline on the actor, one at a time, so tick order equals event order. This ordering and the `at_ms` of each tick are what the replay log records (REP-1). A game's `tick` MUST NOT block, sleep or perform I/O (GAM-2).
- TMR-7: Tests MUST use `tokio::time::pause()` and `advance()`. Tests MUST NOT sleep on the wall clock.
- TMR-8: The inbox is a bounded `mpsc` channel (`inbox_capacity`, default 256). The engine never blocks on outbound events (ARC-8).
- TMR-9: **Time models (Confirmed: depends on the game).** Each game declares a `TimeModel`. `PlayerClocks`: the game keeps per-player clocks inside itself (chess, doushouqi and similar), driven only by `at_ms`, `lag_credit_ms` and `time_limit_ms`. `RoundDeadlineOnly`: no per-player clocks; the round deadline is the only limit, and it is typically the natural end of a game about scoring as much as possible before it (GAM-7).
- TMR-10: A player's clock reaching zero in a `PlayerClocks` game is a rule of that game, not an engine disqualification. The game ends that player's play with a normal outcome (usually a loss). Engine disqualification (`NoResult`) only arises at the hard cap (TMR-4) or when a player leaves (PRS-6).
- TMR-11: **Lag credit.** `lag_credit_ms` in `GameInput::Player` is computed by the server from server-measured round-trip time (LAT-6, LAT-7). The engine MUST clamp it to `LAG_CAP_MS` per input and MUST limit each player's total credit in a round to `LAG_TOTAL_CAP_MS`. The clamped value is what the game receives and what the replay log records.

## 8. Presence, leaving, and removal

- PRS-1: Presence comes only from the `connected` flag, which only `Command::Connection` changes. The engine takes one snapshot of all InMatch players' flags per round start and uses it for that round start. Each seated player is `connected = true` when the match starts.
- PRS-2: At each round start, for every InMatch player: if Present, set `absent_streak = 0`; if not Present, increment `absent_streak`.
- PRS-3: If `absent_streak >= K` after the update, the player becomes `Removed` immediately. Removal is permanent (PRS-4). The engine emits `PlayerStatus::Removed`.
- PRS-4: A Removed player MUST NOT become a participant again, even if later connected. A Removed player scores no points in later rounds, keeps their existing `total_points`, and appears in the final ranking below every player who was not Removed (FIN-1).
- PRS-5: A player who is not Removed and is Present at a round start is a participant, with no further action required. This is how a player who timed out or left in an earlier round returns.
- PRS-6: **Reconnect grace (Confirmed: 30 s, the clock keeps running).** When a participant disconnects mid-round (`Connection { connected: false }`), the engine MUST emit `PlayerStatus::Away` and start a grace period of `RECONNECT_GRACE`; it MUST NOT pause the game or its clocks. If the player reconnects within the grace, the engine emits `PlayerStatus::Present` and the player continues the same round. When the grace expires, the player is round-disqualified for that round only: the engine marks them left and ticks `GameInput::PlayerLeft`; if the game's `result()` still has no outcome for them, the engine records `NoResult(LeftRound)`. If the game does determine an outcome (for example it ranks them and continues), that outcome stands.
- PRS-7: A player whose grace has expired does NOT rejoin the running round: their inputs for the rest of that round MUST be rejected (`PlayerLeftRound`) even if they reconnect, and they may play again from the next round start, subject to PRS-5. A disconnect in `Phase::Lobby` has no grace (LOB-15).
- PRS-8: Round disqualification (timer expiry or leaving mid-round) MUST NOT change `Standing` and MUST NOT change `absent_streak`. Only round-start checks change the streak.
- PRS-9: Round-disqualified players still count as InMatch for the match end condition.
- PRS-10: **Match end condition.** The match ends at a round-start check (RND-2) when the number of InMatch players is 1 or fewer. The match also ends after the last slot. When the match ends early, the remaining InMatch player wins (FIN-1, Confirmed).
- PRS-11: In-game elimination is not a disqualification and MUST NOT be reported as `NoResult`. The game reports the player's normal placement or score.
- PRS-12: Between rounds (phase `RoundFinished`, `Starting`), `Connection` commands only update the `connected` flag and emit `PlayerStatus` (`Present` or `Away`).

## 9. Scoring and ranking

- SCR-1: Scoring is purely rank-based (Confirmed). Raw score magnitudes never enter the formula; scores only decide ordering. Games with different score ranges (max 10 vs max 100) are therefore directly comparable. Games never normalize their own scores.
- SCR-2: The scoring pipeline is: the game produces a `GameResult`; the engine validates it (RND-9); the engine calls `compute_places` and `points_for_place`; match totals are updated; the final ranking is computed (FIN-1). The computation MUST live in `superminigames-logic` (`places_from_keys`, `points_for_place`, `round_points`, `rank_players`); `core` exposes `compute_places` as a thin adapter that maps each `PlayerOutcome` to its key (SCR-3). These functions MUST have no I/O.
- SCR-3: Let `n` be the number of participants. Define each participant's sort key:

| Outcome | Key |
|---|---|
| `Placed(v)` | `(0, v)` |
| `Scored(s)` | `(0, -(s as i128))` so higher scores sort first |
| `NoResult(_)` | `(1, 0)` |

- SCR-4: `place(p) = 1 + (number of participants q with key(q) < key(p))`, comparing keys lexicographically. `group(p)` is the number of participants whose key equals `key(p)` (including `p`). Tied players share a place and the next places are skipped (1, 1, 3, 4).
- SCR-5: `NoResult` players therefore rank below every player who has an outcome and form one tied group. A player who was in-game eliminated or round-disqualified but has an outcome uses that outcome's place (Confirmed).
- SCR-6: **Ties split the points (Confirmed for draws; applied to all ties).** The base points of place `q` are `12 * (n - q) / (n - 1)`. A tied group receives the average of the base points of the places it occupies. In integer arithmetic at scale 12 this is exact for n in 2..=4:

```rust
pub const POINTS_SCALE: u32 = 12;

/// `group` = number of participants tied at `place` (1 if no tie).
pub fn points_for_place(n: usize, place: usize, group: usize) -> u32 {
    debug_assert!((2..=4).contains(&n));
    debug_assert!(place >= 1 && group >= 1 && place + group - 1 <= n);
    let (n, p, g) = (n as u32, place as u32, group as u32);
    6 * (2 * n + 1 - 2 * p - g) / (n - 1)
}
```

- SCR-7: Points per place with no ties (scale 12):

| n | 1st | 2nd | 3rd | 4th |
|---|---|---|---|---|
| 2 | 12 | 0 | - | - |
| 3 | 12 | 6 | 0 | - |
| 4 | 12 | 8 | 4 | 0 |

- SCR-8: Tie examples. n = 2, a draw (both 1st): 6 and 6. n = 4: places 1,1,3,4 give 10,10,4,0; places 1,2,2,4 give 12,6,6,0; places 1,2,3,3 give 12,8,2,2; all four tied gives 6,6,6,6.
- SCR-9: If every participant ends with `NoResult`, the round MUST be recorded as `Skipped(NoResults)` and award 0 points to everyone. Non-participants (absent or Removed) receive 0 points for the round and are not counted in `n`.
- SCR-10: Totals are `u32`. Points are added only for `Scored` rounds. All arithmetic MUST be integer; floating point MUST NOT be used anywhere in scoring or ranking.
- SCR-11: Every `Scored` round awards exactly `6 * n` points in total, whatever the ties, so no round outweighs another.
- SCR-12: Until games for 3 or 4 players exist, `MAX_PLAYERS = 2` and every round has `n = 2` (Confirmed). The scoring functions, tests and proofs remain defined for n in 2..=4 so that raising `MAX_PLAYERS` later needs no scoring change.
- FIN-1: Final ranking orders all seated players by (Removed last, then `total_points` descending) and uses competition ranking over that order. Players share a rank only if they are in the same group and have equal totals (Confirmed: no tiebreaker). A Removed player therefore ranks below every player who was never Removed, whatever the totals, and when a match ends early because the other player was Removed, the remaining player is rank 1. Ties inside the output list are ordered by seat order.

## 10. Failure handling

- FLR-1: If a round attempt's `initialize` returns `Err(GameError::InitFailed(_))` or panics, the engine MUST retry, up to 3 retries. That is at most `START_ATTEMPTS = 4` attempts in total.
- FLR-2: Each retry creates a fresh instance from the factory (RND-12), reuses the same game and game seed (RNG-6), and captures a fresh deadline only once play starts (TMR-1). A retry MUST NOT use randomness. A fixed backoff of up to 1 second MAY be added.
- FLR-3: If all 4 attempts fail, record `Skipped(StartFailed)`. The slot is consumed and the match continues with the next slot.
- FLR-4: If `tick` returns `Err(GameError::Fatal(_))` or panics, the engine MUST discard the instance, record `Skipped(GameAborted)`, and MUST NOT retry. Players may already have seen game events; the engine emits `RoundFinished` with the skip reason.
- FLR-5: An invalid result yields `Skipped(InvalidReport)` (RND-9). No retry.
- FLR-6: `RoundRecord.attempts` MUST hold the number of attempts actually made.
- FLR-7: A panic inside any `Game` method MUST NOT crash the actor or the server. The engine MUST wrap each call to `initialize`, `tick`, `is_finished` and `result` in `std::panic::catch_unwind(AssertUnwindSafe(..))` and map a panic to the failure rules above.
- FLR-8: No engine code path may use `unwrap()` or `expect()` outside tests; use `Result` with typed errors (`thiserror`). Invalid commands return structured errors, never panics.

## 11. Engine architecture

- ARC-1: `MatchEngine` responsibilities: manage lobby state; validate readiness; apply allow/ban filtering; select games; run rounds; enforce timers; track presence, removal and round disqualification; aggregate points; determine final ranking; record replay logs when enabled.
- ARC-2: The engine MUST be a single-owner actor. One tokio task per lobby (and its match) owns all lobby and match state. The server sends `Command`s over a bounded `mpsc` channel, each with a `oneshot` reply carrying `Result<(), EngineError>`. No `Mutex` or `RwLock` around lobby or match state. No shared mutable state between matches.
- ARC-3: The engine publishes `EngineEvent`s on a channel the server consumes. Every observable decision (picks, skips, removals, points) MUST be recoverable from `EngineEvent`s and the final `MatchReport` so tests can assert against them.
- ARC-4: The engine never reads the wall clock directly; it uses `tokio::time` only (TMR-7). `chrono` is used only in the server.
- ARC-5: The phase machine is explicit and every transition is validated (section 12). An invalid transition is a bug and MUST be a typed error, not a panic. Transition validity is decided by `next_phase` in `superminigames-logic` (section 19).
- ARC-6: Suggested engine module layout (illustrative): `lobby.rs`, `registry.rs`, `selection.rs`, `round.rs`, `presence.rs`, `actor.rs`, `report.rs`, `error.rs`. Selection and phase-transition logic SHOULD be pure functions so they can be unit-tested without a runtime.
- ARC-7: No global mutable state. `tracing` output MUST NOT affect behavior.
- ARC-8: The engine MUST NOT block on outbound events. Event delivery uses a channel with bounded capacity; if the server falls behind, the server (not the engine) disconnects the slow consumer (SRV-7).
- ARC-9: The server creates the actor with `EngineInit` (section 3). Entry point (illustrative): `spawn_engine(init) -> Result<EngineHandle, LobbyError>`; `EngineHandle` offers `send(Command) -> Result<(), EngineError>` and `subscribe() -> Receiver<EngineEvent>`.
- ARC-10: **Overload.** If the engine cannot enqueue an `EngineEvent` because its outbound channel is full, the actor MUST NOT block. It treats the lobby as overloaded, stops the match, sets the phase to `Cancelled` and ends, dropping the channel. The server detects the closed channel, closes the lobby's connections with an error, and increments the `engine_overload_total` metric.

## 12. State machines

### 12.1 Engine phase

| From | Trigger | Guard | To |
|---|---|---|---|
| (none) | actor created | config valid | Lobby |
| Lobby | StartMatch | host, at least 2 seated, all Ready, config valid | Starting (slot 0) |
| Lobby | Cancel | host | Cancelled |
| Lobby | host leaves or disconnects | - | Cancelled |
| Starting | presence check | InMatch <= 1 | MatchFinished |
| Starting | slot skipped (NotEnoughPlayers, EmptyPool, StartFailed) | - | RoundFinished |
| Starting | attempt succeeded | - | InRound |
| InRound | game finished, or deadline reached, or all participants left | - | RoundFinished |
| InRound | Fatal error or panic, or invalid result | - | RoundFinished (Skipped) |
| RoundFinished | more slots remain | - | Starting (next slot) |
| RoundFinished | last slot processed | - | MatchFinished |
| MatchFinished, Cancelled | any | - | terminal |

### 12.2 Round slot detail

| Step | Outcome |
|---|---|
| Presence check and removal | If InMatch <= 1: MatchFinished (`ended_early`). Otherwise continue. |
| Participants computed | If fewer than 2: Skipped(NotEnoughPlayers). |
| Pool computed | If empty: Skipped(EmptyPool). |
| Selection and game seed | One selection draw (RNG-4), one game-seed draw (RNG-10). |
| Attempt 1..4 | InitFailed or panic: retry. After attempt 4: Skipped(StartFailed). |
| InRound | Ticks until `is_finished()`, deadline, or all participants left. |
| Collect and validate | Invalid: Skipped(InvalidReport). All NoResult: Skipped(NoResults). |
| Scoring | Scored: totals updated. |

### 12.3 Player standing

| State | Event at round start | Next state |
|---|---|---|
| InMatch(streak s) | Present | InMatch(0) |
| InMatch(streak s) | Absent, s + 1 < K | InMatch(s + 1) |
| InMatch(streak s) | Absent, s + 1 >= K | Removed |
| Removed | any | Removed (terminal) |

Round disqualification is NOT a `Standing` state; it exists only inside that round's `RoundRecord`.

## 13. Game runtime, determinism, and replay

- GAM-1: A game is a pure state machine. Given the same `seed`, the same `players` slice, the same `time_limit_ms` and the same ordered `(at_ms, input)` pairs, it MUST produce the same events and the same `result()`.
- GAM-2: A game MUST NOT use: wall-clock time, threads, I/O, `thread_rng` or any OS randomness, global mutable state, or `HashMap`/`HashSet` iteration order (use `BTreeMap` or `Vec`). A game MUST NOT block or sleep inside `tick`. A game reads time only from the `at_ms` and `time_limit_ms` values it is given.
- GAM-3: All game randomness MUST come from the `seed` passed to `initialize`, through the shared helper `chacha_from_seed` in `core`.
- GAM-4: `initialize` MUST fully reset all state. The engine always passes a fresh instance, but a game MUST NOT rely on that.
- GAM-5: `id`, `name`, `scoring_mode` and `supports_players` MUST be pure: no dependence on instance state. `initialize` with an unsupported player count MUST return `GameError::InitFailed`.
- GAM-6: `tick` MUST NOT panic on any input, including malformed `GameAction` payloads. Bad inputs are rejected with `GameEvent::InputRejected`. Internal bugs return `GameError::Fatal`.
- GAM-7: `result()` returns the outcomes the game considers determined or is willing to rank right now. It MUST NOT mutate state and MUST be callable at any time after `initialize`. A game MAY include provisional outcomes for unfinished players (for example ranking survivors by material); this is the game's decision (Confirmed: it depends on the board game). Players omitted from `result()` become round-disqualified (RND-9). When `is_finished()` is `true`, `result()` MUST include every player. For a `RoundDeadlineOnly` game that scores points until the deadline, the deadline is its natural end: `result()` at the deadline MUST include every participant with a `Scored` outcome (players who did nothing score 0), so nobody is disqualified.
- GAM-8: `is_finished()` is monotonic (once `true`, always `true`). After the game is finished, `tick` MUST ignore further inputs and return an empty event list.
- GAM-9: A game MAY keep a player in play after `PlayerLeft` (a future 4-player chess would continue) and MAY assign them a placement. If it does not, they are `NoResult(LeftRound)`.
- GAM-10: `GameInput`, `GameEvent` and `GameResult` are serializable. A game's internal state SHOULD be serializable for debugging but this is not required.
- GAM-11: Every game crate MUST pass the shared conformance tests (T-GAM-1 to T-GAM-5). `core` SHOULD provide them behind a `test-support` feature so every game crate runs the same suite.
- GAM-12: A `PlayerClocks` game MUST derive its clock parameters only from `time_limit_ms` and its own constants, MUST implement `next_wakeup_ms` so the engine can wake it when a clock would run out, and MUST treat a clock reaching zero as a normal game result (usually a loss), not as an engine disqualification (TMR-10).
- GAM-13: A move that arrives when it is not the sender's turn, or that is illegal in the current position, MUST be rejected with `GameEvent::InputRejected` and MUST NOT cost the sender clock time or any penalty. This is what makes client-side premoves safe (LAT-4).
- REP-1: When `EngineOptions.record_replays` is `true`, the engine records a `ReplayLog` for each round that ran: `game`, `game_seed`, `players` (first-mover order), `time_limit_ms`, and every `(at_ms, GameInput)` pair in the exact order ticked, including `PlayerLeft` and `TimeAdvanced`.
- REP-2: Replaying a log (fresh instance, `initialize(game_seed, players, time_limit_ms)`, then each `(at_ms, input)` in order) MUST reproduce the same events and the same `result()` (T-REP-1).
- REP-3: Replay logs are off by default and are persisted only if enabled (STO-3).

## 14. Protocol

- PRO-1: All wire types live in `superminigames-protocol`. They derive `Serialize` and `Deserialize`. The default encoding is MessagePack in WebSocket binary frames (`rmp-serde`); JSON exists only behind the optional `json` feature for debugging. Opaque game payloads travel as raw bytes (`serde_bytes`), never as arrays of integers.
- PRO-2: Messages are versioned explicitly. Every message is wrapped in `Envelope { v: u16, msg }` with `v == PROTOCOL_VERSION`. The server MUST reject an unsupported version with `ServerMessage::Error { code: ProtocolMismatch, .. }` and close the connection. Adding optional fields is compatible. Removing or renaming a field or variant requires a version bump.
- PRO-3: Messages encode with `rmp_serde::to_vec_named` (maps with field names), so optional fields can be added compatibly (PRO-2). Enums use serde's default external tagging.
- PRO-4: Engine-internal state, RNG state and replay logs MUST NOT appear in protocol types. The server owns the mapping between protocol types and engine types.
- PRO-5: The protocol is command/event based. Clients send commands. The server validates and broadcasts authoritative events. Clients MUST NOT send scores, places or outcomes; the server MUST NOT accept them.
- PRO-6: Client to server (`ClientMessage`): `CreateLobby { config, visibility }` (`config` is a `LobbyConfig`, PRO-16), `JoinLobby { lobby_id, code }`, `ListPublicLobbies`, `JoinQueue { preset }`, `LeaveQueue`, `LeaveLobby`, `SetReady { ready }`, `UpdateSettings { update }`, `StartMatch`, `CancelLobby`, `GameInput { action }`, `Ping { nonce }`.
- PRO-7: Server to client (`ServerMessage`): `LobbyState { snapshot }`, `PublicLobbies { lobbies }`, `QueueStatus { state, waited_secs }`, `MatchFound { lobby_id, accept_secs }`, `RoundStarting { round_index, game_id, game_name, participants, time_limit_secs }`, `GameEvent { payload }`, `RoundScored { round_index, placements, points }`, `RoundSkipped { round_index, reason }`, `ScoreUpdate { totals }`, `PlayerStatus { player, status }`, `Resync { phase, round_index, is_participant }`, `MatchFinished { ranking }`, `Error { code, message }`, `Pong { nonce }`.
- PRO-8: Errors are structured. `ErrorCode` is an enum mapped from `LobbyError`, `InputError` and `ConfigError`, plus the server-level codes `ProtocolMismatch`, `MalformedMessage`, `RateLimited`, `ServerBusy`, `Unauthorized` and `AlreadyInLobby`, and for the HTTP auth endpoints `InvalidCredentials`, `InvalidUsername`, `WeakPassword`, `UsernameTaken`, `InvalidEmail`, `EmailNotVerified` and `InvalidToken`. `message` is human-readable text only.
- PRO-9: `GameEvent::ToPlayer` payloads MUST be delivered only to that player. Broadcast payloads go to all seated players.
- PRO-10: The server MUST reject messages larger than 64 KiB (Default) with `MalformedMessage`.
- PRO-11: Transport is WebSocket over TLS 1.3 (SEC-1). The server MUST set `TCP_NODELAY`, MUST NOT batch or delay frames, and MUST NOT compress game frames (LAT-1).
- PRO-12: There is no premove message. Premoves are a client-side queue that sends an ordinary `GameInput` (LAT-4).
- PRO-13: Authentication uses HTTPS endpoints (`POST /register`, `POST /login`, `POST /logout`, `POST /email/verify`, `POST /email/resend`, `POST /password/forgot`, `POST /password/reset`, `POST /password/change`) outside the game protocol. The WebSocket upgrade is authenticated by the session cookie or a short-lived single-use ticket (SEC-7). Passwords never travel over the WebSocket.
- PRO-14: **Resync.** When an authenticated connection is attached to an existing lobby (first connection or reconnect), the server MUST send, in order: `LobbyState`, `Resync { phase, round_index, is_participant }`, `ScoreUpdate`, and one `PlayerStatus` per player. `phase` is one of `lobby`, `starting`, `in_round`, `round_finished`, `match_finished`, `cancelled`; `round_index` is the current or last processed slot; `is_participant` says whether this player is playing the current round (false after leaving it, PRS-7). A client that reconnects mid-round is not put back into the round and learns this from `Resync`.
- PRO-15: `PublicLobbies` carries `PublicLobby { lobby_id, host_name, seats_taken, max_seats, rounds, round_time_limit_secs }` entries. An entry MUST NOT contain a join code, a seed, or any player id.
- PRO-16: `LobbyConfig` is `MatchConfig` without `seed`. Client-to-server message types MUST use `#[serde(deny_unknown_fields)]`, so a `CreateLobby` or `UpdateSettings` carrying a seed is rejected as `MalformedMessage`.

## 15. Server, storage, and observability

### 15.1 Server

- SRV-1: The server spawns one engine actor per lobby (ARC-2). The server assigns `LobbyId` and `MatchId` (`Uuid::new_v4` is allowed here) and passes them in `EngineInit`. The actor and its handle are dropped when the phase reaches `Cancelled` or `MatchFinished` and the result has been handed to storage.
- SRV-2: Auth and sessions: accounts use username and password, and all credential handling follows section 21. The server authenticates each connection and resolves it to a `PlayerId`; the engine trusts the `PlayerId` in commands. A session token MUST let a player resume as the same `PlayerId` after a disconnect.
- SRV-3: Presence: the server turns transport state into `Command::Connection`. Default: the server sends WebSocket ping frames every 5 seconds (every 1 second during a round, LAT-6); a connection is considered lost after 15 seconds with no frame; an authenticated reconnect sends `connected: true`.
- SRV-4: Join codes: the server generates 6-character codes from A-Z and 0-9 using OS randomness. The server MUST rate-limit failed join attempts (Default: 5 per minute per connection and per IP, then a temporary block) and answer `RateLimited`. This is the only place non-deterministic randomness is allowed (RNG-7).
- SRV-5: The server MUST let clients list open public lobbies (`ListPublicLobbies`). Private lobbies MUST NOT be listed.
- SRV-6: The server validates message schema, size and rate before forwarding to the engine; the engine validates semantics.
- SRV-7: Outbound buffers are bounded. A client that falls behind MUST be disconnected by the server; the engine is never blocked.
- SRV-8: A match in progress when the server restarts is not resumed (transient round state is not persisted, STO-4).
- SRV-9: The server measures round-trip time per LAT-6 and attaches `lag_credit_ms` (LAT-7) to every `Command::Input`.
- SRV-10: **Match seed.** At `StartMatch` the server draws a fresh 64-bit seed from the OS CSPRNG and passes it in `Command::StartMatch`. A seed is never taken from client input, so no player can bias game selection or the first mover (RNG-11). The seed is not sent to clients before the match is finished. Tests and debug builds MAY inject a fixed seed through server configuration.
- SRV-11: **One lobby per account.** A player may be seated in at most one lobby at a time. `CreateLobby` or `JoinLobby` while seated elsewhere fails with `AlreadyInLobby`. The server enforces this with an index from `PlayerId` to `LobbyId` in its lobby directory.

### 15.2 Storage

- STO-1: Persistence lives only in the server's storage module behind a `Storage` trait. `core`, `engine`, games and `client` MUST NOT touch a database.
- STO-2: Persisted by default: the `MatchReport` (match id, lobby id, config including the seed, per-round records without replay logs, final ranking), server-added UTC timestamps (`chrono::DateTime<Utc>`), and player accounts if the auth layer uses them.
- STO-3: Replay logs are persisted only when replay recording is enabled.
- STO-4: Transient round state is NOT persisted.
- STO-5: The database layer uses `sqlx`. The database engine and schema are not specified here (SPEC-GAP, SCP-2).
- STO-6: Storage failures MUST NOT affect a running match. Persist after `MatchFinished`, retry with backoff on failure, and log errors. The engine actor never waits on storage.

### 15.3 Observability

- OBS-1: Use `tracing` with structured logs. Every span and event of a match MUST include `match_id` and `lobby_id` as correlation ids.
- OBS-2: Emit a round event log per round: index, game id, participants, attempts, status, duration.
- OBS-3: Metrics: lobby creation count; round duration; disconnect rate; match completion rate; game selection distribution (counter per `GameId`).
- OBS-4: The metrics exposition format is out of scope.

## 16. Worked example (informative)

Setup: players A and B (2 players only, SCR-12). K = 3. `rounds = 4`. Points use scale 12. The games are named after their type; the actual picks depend on the seed.

| Round | Events | Places | Points | Totals after (A / B) |
|---|---|---|---|---|
| 0 | Both connected. Chess (`PlayerClocks`). A checkmates B: A Placed(1), B Placed(2). | A=1, B=2 | A 12, B 0 | 12 / 0 |
| 1 | Shogi. Draw by repetition: A Placed(1), B Placed(1). The draw splits the points (SCR-6). | A=1, B=1 (group 2) | A 6, B 6 | 18 / 6 |
| 2 | A hypothetical points-collecting game (`RoundDeadlineOnly`, HigherIsBetter). At the deadline `result()` has A Scored(4), B Scored(9). | B=1, A=2 | B 12, A 0 | 18 / 18 |
| 3 | Xiangqi. B disconnects mid-round and does not return within the 30 s grace: the engine ticks `PlayerLeft(B)` and the game ends with B `NoResult(LeftRound)` and A Placed(1). | A=1, B=2 | A 12, B 0 | 30 / 18 |

Final ranking: A 30 (rank 1), B 18 (rank 2).

Notes:
- If a chess clock reaches zero, that is a normal loss from the game (TMR-10), not an engine disqualification.
- With 7 or more rounds, if B stayed disconnected at the starts of rounds 4, 5 and 6: rounds 4 and 5 are `Skipped(NotEnoughPlayers)` (only A is present), B's streak reaches 3 at the round-6 start and B becomes Removed, and the match ends there with `ended_early = true`. A is ranked first because B was Removed (FIN-1).

## 17. Acceptance tests

Each test is Given / When / Then. Test names SHOULD include the ID. Verification beyond these tests (Verus, fuzzing, benchmarks) is defined in section 19.

### Workspace

- T-WSP-1: Given the workspace, when CI runs the dependency check, then every crate depends only on what the table in section 1.1 allows, and `core` and `protocol` build for `wasm32-unknown-unknown` (WSP-3, WSP-6, PRO-4, STO-1, VER-11).

### Lobby

- T-LOB-1: Given lobby with host H and player P and nobody Ready, when H updates settings, then accepted. When P sets Ready and H updates settings, then `SettingsLocked`. When P unreadies and H updates settings, then accepted (LOB-6).
- T-LOB-2: Given private lobby with code `ABC123`, when a player joins with no code or `ABC124`, then `InvalidJoinCode`; with `ABC123`, then accepted (LOB-3).
- T-LOB-3: Given 2 seated players, when a 3rd joins, then `LobbyFull` (LOB-2).
- T-LOB-4: Given H and P where only H is Ready, when H sends StartMatch, then `NotAllReady`. When P also readies, then accepted and the phase is `Starting` (LOB-9, LOB-1).
- T-LOB-5: Given only the host seated and Ready, when StartMatch, then `NotEnoughPlayers` (LOB-9).
- T-LOB-6: Given a lobby in `Phase::Lobby`, when the host leaves, then the phase is `Cancelled` and further commands fail (LOB-11).
- T-LOB-7: Given a match in progress, when Join, SetReady, UpdateSettings, Cancel or StartMatch arrives, then `WrongState` (LOB-4, LOB-10, LOB-14).
- T-LOB-8: Given filter inputs: an allow list with an unknown id gives `UnknownGame`; an empty allow list gives `EmptyAllowList`; a list with a duplicate gives `DuplicateGameInList`; a ban list covering every registered game gives `NoEligibleGames` (LOB-12, LOB-8).
- T-LOB-9: Given a seated non-host player in `Phase::Lobby`, when `Connection { connected: false }` arrives, then their seat is freed exactly as with `Leave` (LOB-15).
- T-LOB-10: Given a lobby with no commands for `LOBBY_IDLE_TIMEOUT` (paused time), then the phase becomes `Cancelled` (LOB-16).
- T-LOB-11: Given `StartMatch` with seed s, then `config.seed == s` in the `MatchReport`, and `UpdateSettings` has no seed field (LOB-6, LOB-9).
- T-LOB-12: Given a `SetReady` and an `UpdateSettings` sent at the same time, then the outcome equals processing them in arrival order, and the loser gets its normal error (LOB-13).

### Selection and randomness

- T-RNG-1 (golden selection): Given seed 0 and a registry sorted as [A,B,C,D,E], when picks are made for round indices 0..=9, then the sequence equals the committed file `tests/golden_seed0.txt`. The file is generated once by the reference implementation, human-reviewed, and committed; any later change MUST fail CI (RNG-9, RNG-1).
- T-RNG-2 (uniformity): Given a 3-game pool and seed 0, when picks are made for round indices 0..30000, then each game's count is within 5% of 10000. This is deterministic and MUST NOT flake (RNG-5).
- T-RNG-3: Given the same games registered in different orders, then picks for every round are identical (PLL-1 sorting).
- T-RNG-4: Given a round whose first 2 attempts fail with `InitFailed`, then the game finally played is the same one chosen with no failures, and it received the same `game_seed` (RNG-6).
- T-RNG-5: Given rounds 0-4 played, skipped or failed in different ways, then the pick for round 5 is identical in all cases (RNG-3, RNG-4).
- T-RNG-6 (golden game seed): Given seed 0, when `game_seed` is derived for rounds 0..=4, then the values equal the committed file `tests/golden_game_seed0.txt` (RNG-10).
- T-RNG-7: Given seed 0, then the opening bit equals the value committed in `tests/golden_seed0.txt` (RNG-11).
- T-POOL-1: Given allow list [A, C], then only A or C is ever picked. Given ban list [B], then B is never picked.
- T-POOL-2: Given 2 participants and a registry in which only game X supports 2 players, then X is picked every round. If no game supports 2 players, then the round is `Skipped(EmptyPool)`.
- T-POOL-3: Given `register(factory)` twice with the same id, then `DuplicateGameId`; given any registry, then it is sorted by `GameId` and the registered prototype is never initialized or ticked (TYP-2, PLL-1).

### Scoring and ranking

- T-SCR-1: Given no ties, then points (scale 12) match the table in SCR-7 for n = 2, 3, 4.
- T-SCR-2: Given n = 2 and both players Placed(1) (a draw), then points are (6,6). Given n = 4 and Placed values (1,1,3,4), then (10,10,4,0). Given (1,2,2,4), then (12,6,6,0). Given (1,1,1,1), then (6,6,6,6) (SCR-8).
- T-SCR-3: Given a HigherIsBetter game with scores 12, 11, 3, 0 (n = 4), then places are 1, 2, 3, 4. Given scores 5, 5, 0, 0, then (place, group) are (1,2), (1,2), (3,2), (3,2) and points 10, 10, 2, 2. Given the same game with every score multiplied by 10, then points are unchanged (SCR-1).
- T-SCR-4: Given 4 players where A is Placed(1), B is Placed(2), and C and D are NoResult, then points are 12, 8, 2, 2 (SCR-5).
- T-SCR-5: Given every participant is NoResult, then the round is `Skipped(NoResults)` with 0 points for all (SCR-9).
- T-SCR-6: Given a Placement game that reports `Scored`, or a HigherIsBetter game that reports `Placed`, or a duplicate player, or `Placed(0)`, or a finished game whose result omits a participant, then `Skipped(InvalidReport)` (RND-9).
- T-SCR-7: Given a 4-player game at function level (no 4-player game exists yet) where a player is eliminated early but the game continues to the end and ranks that player 2nd, then that player receives 8 points (SCR-5).
- T-SCR-8: Given final totals 15, 15, 9, 3, then ranks are 1, 1, 3, 4 (FIN-1).
- T-SCR-9: Given a Removed player with 9 points and a remaining player with 3, then the remaining player is rank 1 and the Removed player rank 2 and keeps 9 points in the report (FIN-1, PRS-4).
- T-SCR-10: Given a game that returns provisional outcomes for unfinished players when the deadline passes, then the engine uses them as given; players omitted are NoResult and rank below all with outcomes (GAM-7).
- T-SCR-11: Given any valid keys for n in 2..=4, then the round's points sum to exactly `6 * n` (SCR-11).

### Presence and removal

- T-PRS-1: Given a participant who disconnects during round r and does not return within `RECONNECT_GRACE`, then `tick(PlayerLeft)` is delivered exactly once, after the grace, and they are `NoResult(LeftRound)` for r only (unless the game ranks them). If they are connected at the start of r+1, then they are a participant in r+1 (PRS-5, PRS-6).
- T-PRS-2: Given K = 3 and a player not connected at the starts of rounds 5, 6, 7, then they are Removed at the start of round 7, not before (PRS-3).
- T-PRS-3: Given a player absent at rounds 5 and 6 and connected at round 7, then the streak resets; absent again at 8 and 9 does not remove them (PRS-2).
- T-PRS-4: Given a Removed player who reconnects at a later round start, then they are not a participant (PRS-4).
- T-PRS-5: Given 2 InMatch players where 1 is Removed at a round start, then the phase becomes `MatchFinished` at that start with `ended_early = true`, later slots are not run, and the remaining player is rank 1 even if their total is lower (PRS-10, FIN-1).
- T-PRS-6: Given 2 InMatch players with only 1 connected at a round start, then the round is `Skipped(NotEnoughPlayers)`, the slot is consumed, no points are awarded, and the absent player's streak increases (RND-3).
- T-PRS-7: Given a player who timed out in a round, then they remain InMatch and `absent_streak` is unchanged (PRS-8, PRS-9, TMR-5).
- T-PRS-8: Given a participant who disconnects and reconnects within `RECONNECT_GRACE`, then no `PlayerLeft` is ticked, the clocks are never paused, the round continues and their later inputs are accepted; given they reconnect after the grace, then their inputs are rejected with `PlayerLeftRound` (PRS-6, PRS-7).
- T-PRS-9: Given a participant who disconnects after the presence snapshot but before `initialize` returns, then their grace starts at `at_ms = 0` and `PlayerLeft` is ticked only if they have not returned when it expires (RND-13).
- T-PRS-10: Given every participant's grace expires mid-round, then the round ends immediately without waiting for the deadline (RND-13).
- T-PRS-11: Given a participant disconnects mid-round, then `PlayerStatus::Away` is emitted at once, and `PlayerStatus::Present` on a reconnect within the grace (PRS-6).

### Timers and in-round loop

- T-TMR-1: Given paused time and a game that never finishes, when the deadline passes, then `result()` is called exactly once and missing players are `NoResult(TimerExpired)` (TMR-4).
- T-TMR-2: Given the deadline has passed, when an `Input` arrives, then it is rejected with `NotInRound` and no `tick` happens (TMR-4).
- T-TMR-3: Given a game that finishes before the deadline, then the round ends at that tick and no timer expiry is recorded (TMR-3).
- T-TMR-4: Given inputs from two players arriving in order I1, I2, then ticks happen in that order and the replay log lists them in that order (TMR-6, REP-1).
- T-TMR-5: Given a `RoundDeadlineOnly` game, when the deadline passes, then `result()` includes every participant with a `Scored` outcome and nobody is `NoResult` (GAM-7, TMR-9).
- T-TMR-6: Given a game whose `next_wakeup_ms` returns t, then with paused time the engine ticks `TimeAdvanced` at exactly `round_start + t` and re-reads `next_wakeup_ms` after every tick (TMR-2).
- T-TMR-7: Given inputs whose `lag_credit_ms` exceeds `LAG_CAP_MS`, or whose running total exceeds `LAG_TOTAL_CAP_MS`, then the game receives the clamped values and the replay log records them (TMR-11).
- T-TMR-8: Given an `Input` whose `received_at` is earlier than the dequeue time, then the game receives `at_ms` computed from `received_at`, never lower than the previous tick's `at_ms` (TMR-1).

### Failure handling

- T-FLR-1: Given `InitFailed` three times then success, then the round is `Scored`, `attempts == 4`, and the factory was called once per attempt (FLR-1, RND-12, FLR-2).
- T-FLR-2: Given `InitFailed` four times, then the round is `Skipped(StartFailed)`, exactly 4 attempts were made, and the next slot runs normally (FLR-3, FLR-6).
- T-FLR-3: Given `tick` returns `Fatal`, then `Skipped(GameAborted)` with no retry (FLR-4).
- T-FLR-4: Given a game that panics in `tick` or `initialize`, then the actor survives, the round is handled per FLR-1 or FLR-4, and the next slot runs (FLR-7).

### Game runtime and replay

- T-GAM-1: Given any game and a scripted input sequence, when run twice with the same seed and players, then events and `result()` are identical (GAM-1, GAM-3).
- T-GAM-2: Given any game, then `id`, `name`, `scoring_mode` and `supports_players` return the same values before and after `initialize` and after any ticks (GAM-5).
- T-GAM-3: Given any game and malformed `GameAction` payloads (null, wrong types, huge strings), then `tick` never panics and rejects with `InputRejected` (GAM-6).
- T-GAM-4: Given any game, when `initialize` is called twice with different seeds on the same instance, then state is fully reset (GAM-4). When it is called with an unsupported player count, then `InitFailed` (GAM-5).
- T-GAM-5: Given any finished game, then `is_finished()` stays `true`, further ticks return no events, and `result()` contains every player (GAM-7, GAM-8).
- T-GAM-7: Given a fake 3-player game where a player is eliminated by its rules but play continues, then the game reports a placement for that player and never `NoResult` (PRS-11).
- T-REP-1: Given a recorded `ReplayLog` from a round, when replayed on a fresh instance, then events and `result()` equal the original (REP-2).

### Protocol

- T-PRO-1: Given every `ClientMessage` and `ServerMessage` variant, when serialized and deserialized, then the value round-trips; and golden binary fixtures in `tests/protocol/` match byte for byte (PRO-1, PRO-3).
- T-PRO-2: Given an `Envelope` with a version other than `PROTOCOL_VERSION`, then the server replies `ProtocolMismatch` and closes (PRO-2).
- T-PRO-3: Given a client that sends a score or outcome message, then the server rejects it and the engine never sees it (PRO-5).
- T-PRO-4: Given a `GameEvent::ToPlayer` for player A, then player B's connection never receives it (PRO-9).
- T-PRO-5: Given a message over 64 KiB, then `MalformedMessage` (PRO-10).
- T-PRO-6: Given a reconnect to a lobby mid-round, then the server sends `LobbyState`, `Resync` (with `is_participant = false` for a player who left the round), `ScoreUpdate`, then `PlayerStatus` per player, in that order (PRO-14).
- T-PRO-7: Given a `CreateLobby` or `UpdateSettings` message carrying a `seed` field, then it is rejected as `MalformedMessage`; and a `PublicLobbies` entry contains none of join code, seed or player id (PRO-15, PRO-16).

### Server and storage

- T-SRV-1: Given a connection that stops sending frames for 15 seconds (paused time), then the server sends `Connection { connected: false }` (SRV-3).
- T-SRV-2: Given 6 failed join attempts within a minute from one connection, then the 6th is answered `RateLimited` (SRV-4).
- T-SRV-3: Given private and public lobbies, then `ListPublicLobbies` returns only the public ones, each with the PRO-15 fields (SRV-5, PRO-15).
- T-SRV-4: Given a player seated in a lobby, when they send `CreateLobby` or `JoinLobby` for another, then `AlreadyInLobby` (SRV-11).
- T-SRV-5: Given two `StartMatch` commands in two lobbies, then the seeds passed to the engines come from the server's CSPRNG and differ, and no client-supplied seed is used (SRV-10, SEC-18).
- T-SRV-6: Given a valid session token, when a player reconnects after a disconnect, then they resume as the same `PlayerId` (SRV-2).
- T-SRV-7: Given a client that stops reading, then its outbound queue fills, the server disconnects it, and the engine is never blocked (SRV-7, ARC-8).
- T-STO-1: Given a finished match and a storage backend that fails, then the match still completes normally and the result is retried (STO-6).
- T-STO-2: Given a finished match with replay recording off, then no replay log is stored (STO-3).
- T-OBS-1: Given a running match, then every log event and span carries `match_id` and `lobby_id` (OBS-1).

### Match

- T-MTH-1: Given `rounds = 5` with 2 skipped rounds, then the `MatchReport` contains exactly 5 `RoundRecord`s with indices 0..4 (MTH-1).
- T-MTH-2: The worked example in section 16 reproduces its totals and final ranking exactly, using scripted games (MTH-2, MTH-3).
- T-MTH-3: Given a match where several games start and one round is skipped, then the first mover alternates between the two players for each game that started, the first game's first mover equals the opening bit, and the skipped round does not change the alternation (RNG-11, RND-11).
- T-ARC-1: Given a subscriber that never reads and a full event channel, then the actor stops without blocking, the phase becomes `Cancelled`, and the server closes the lobby's connections and counts the overload (ARC-10, ARC-8).
- T-ARC-2: Given a full match, then every pick, skip, removal and point in the `MatchReport` can be rebuilt from the emitted `EngineEvent`s (ARC-3).
- T-ARC-3: Given a transition that `next_phase` does not allow, then the engine returns a typed error and does not panic (ARC-5).
- T-ARC-4: Given the same match run with tracing on and off, then the `MatchReport`s are identical (ARC-7).

## 18. Decision log (informative)

| ID | Decision | Status | Alternative if you want to change it |
|---|---|---|---|
| DEC-1 | Pure rank-based scoring; raw scores only order players. The implementation spec's "normalizer to match score units" step is replaced by `compute_places` and `points_for_place`. | Confirmed | Blend in score margins. |
| DEC-2 | Ties share the place and that place's points (1,1,3,4). | Superseded by DEC-38 | - |
| DEC-3 | No lower-is-better games. | Confirmed | Add a `LowerIsBetter` mode. |
| DEC-4 | Players disqualified or eliminated in-game keep the points for the place the game gives them. | Confirmed | - |
| DEC-5 | Leaving or timing out disqualifies for that round only; return before the next round start is allowed. A mid-round disconnect first gets a 30 s reconnect grace (DEC-64). | Confirmed | - |
| DEC-6 | K = 3 consecutive absent round starts removes a player permanently. | Confirmed | Different K, or a time-based rule. |
| DEC-7 | Points scale is 12 (exact integers for n = 2..4 when ties are averaged). | Default | Any scale divisible by 12. |
| DEC-8 | `NoResult` players rank below all players with outcomes and tie with each other. | Default | Flat 0 points regardless of other players. |
| DEC-9 | A round where everyone is `NoResult` is skipped with 0 points. | Default | Treat as all tied for first. |
| DEC-10 | At the deadline the game decides, through `result()`, whether unfinished players get provisional outcomes. Omitted players are `NoResult`. | Default | Engine ranks unfinished players above players the game already eliminated. |
| DEC-11 | Skipped rounds consume their slot and count toward `rounds`. | Default | Skipped rounds do not count (match may run longer). |
| DEC-12 | Retries reuse the same game and game seed; fresh instance each attempt; 4 attempts total. | Default | Re-draw a game after each failure. |
| DEC-13 | The host must press Ready and starts the match with `StartMatch`. | Default | Auto-start when all players are Ready. |
| DEC-14 | Settings unlock again when all players unready. | Default | Lock for good after the first Ready. |
| DEC-15 | Host leaving cancels the lobby; no cancel after start. | Default | Transfer host; allow cancel mid-match. |
| DEC-16 | No new players may join after the match starts. | Default | Allow late joiners with 0 points. |
| DEC-17 | One hard deadline per round, enforced by the engine. Games with per-player clocks keep them inside the game (DEC-40). | Default | Per-player clocks owned by the engine. |
| DEC-18 | Config bounds: rounds 1..=100, time limit 1..=3600 s, K 1..=10. | Default | Any other bounds. |
| DEC-19 | Removed players stay in the final ranking below every player still in the match; when a match ends early the remaining player wins. | Confirmed | Rank by totals only. |
| DEC-20 | ChaCha8 with one stream per round and hand-written rejection sampling. | Default | Single shared stream across rounds. |
| DEC-21 | The tick-based, synchronous `Game` trait from the implementation spec replaces my earlier async `play`. Changes made to it: `initialize` and `tick` return `Result`; `tick` returns `Vec<GameEvent>`; added `scoring_mode`; `GameInput`, `GameEvent`, `GameResult` defined; `result()` semantics defined. | Adapted from your implementation spec | Async `play` with a cancellation token. |
| DEC-22 | A new `superminigames-protocol` crate holds wire types, so the server does not depend on the UI-facing client crate. | Default | Keep protocol types inside `client`. |
| DEC-23 | `core` contains types, the `Game` trait, thin scoring adapters over `logic` and the shared RNG helpers, and no game rules (the implementation spec said both "core has game rules" and "game rules live in game crates"). `core` may depend on `rand_core` and `rand_chacha`. | Default | Put RNG helpers in a separate tiny crate. |
| DEC-24 | Ids are `Uuid` newtypes (`PlayerId`, `LobbyId`, `MatchId`); `GameId` is a string newtype so ordering is human-readable. The server generates ids. | Default | Integer ids. |
| DEC-25 | The engine tracks presence through `Command::Connection` events. The server owns heartbeats (ping 5 s, lost after 15 s). | Default | Engine polls a presence provider. |
| DEC-26 | In the lobby, a disconnect is treated as `Leave`. | Default | Grace period before freeing the seat. |
| DEC-27 | The server lists open public lobbies; private lobbies are never listed. | Default | Public lobbies joinable by id only. |
| DEC-28 | Superseded by DEC-40: games may keep per-player clocks, driven by engine-supplied timestamps. | Superseded | - |
| DEC-29 | A match in progress is not resumed after a server restart. | Default | Persist round state and resume. |
| DEC-30 | The `rand` crate is allowed only in the server (join codes). Everything deterministic uses `rand_core` and `rand_chacha`. | Default | Allow `rand` everywhere with a fixed algorithm. |
| DEC-31 | Not decided by this spec: database engine and schema, UI frameworks for web/desktop/mobile, metrics exposition format, OAuth or guest login, the email provider, multi-region deployment. | Open | Decide and add to the spec. |
| DEC-32 | Verification policy adopted: editor hooks by default; Verus, fuzzing and Criterion only by trigger. Verus is limited to a new pure crate `superminigames-logic`, because `cargo verus` verifies crates that opt in and supports only a subset of Rust. Async, tokio, serde and `dyn Game` code is covered by tests and fuzzing, not Verus. | Default | Verify more crates, or split `logic` into several crates. |
| DEC-33 | Your policy says to fix the proof or the spec when a proof is impossible. Limited here: the AI may propose a formal-spec change, but a human approves it, and `assume`, `admit`, `external_body` and `external` are banned in `logic` without an approval marker. | Default | Let the AI change formal specs freely. |
| DEC-34 | Fuzz targets are a fixed list (VER-21), always time-boxed: 60 s per affected target per change, 10 min per target in nightly CI. | Default | Different budgets. |
| DEC-35 | "Feature-complete" for Criterion means all section 17 tests are implemented and passing. | Default | Another milestone. |
| DEC-36 | `#![forbid(unsafe_code)]` in every crate; Miri is not used. | Default | Allow unsafe in named modules and add Miri. |
| DEC-37 | `proptest` property tests are recommended inside the normal `cargo test` hook for scoring, ranking and presence invariants. | Default | Skip property tests. |
| DEC-38 | Ties split the points: a tied group gets the average of the points of the places it occupies (a draw in a 2-player round is 6 each at scale 12). You decided this for draws; I applied it to all ties, replacing the earlier rule where tied players each got the full points of the shared place. | Confirmed for draws, Default for other ties | Draw-only splitting, with other ties at full place points. |
| DEC-39 | Matches are 2 players only for now (`MAX_PLAYERS = 2`). Scoring functions and tests stay defined for 2 to 4. | Confirmed | Pair players each round, or round-robin, once 3-4 player games exist. |
| DEC-40 | Time model is per game (`PlayerClocks` or `RoundDeadlineOnly`) and the round deadline is always a hard cap. The `Game` trait gains `time_model`, `next_wakeup_ms`, `at_ms`, `time_limit_ms` and the `TimeAdvanced` input. | Confirmed (model); Default (API) | Engine-owned clocks. |
| DEC-41 | Game payloads are opaque binary (`Vec<u8>`) in each game's own compact encoding. The wire format is MessagePack; JSON only behind a debug feature. | Default | JSON everywhere. |
| DEC-42 | Lag compensation: server-measured RTT (EWMA, ping every 1 s during a round), credit capped at 1000 ms per move and 10 s per round per player; flag-fall grace equals the per-move cap. | Default | Other caps, or no compensation. |
| DEC-43 | Premoves are a client-side queue; there is no premove protocol message. | Default | Server-side premove queue. |
| DEC-44 | Capacity target: 50,000 concurrent matches on one 8-core, 16 GiB instance, at most 100 KiB per match, server processing p99 at most 1 ms. | Default | Other targets. |
| DEC-45 | Accounts use username and password with Argon2id hashing, hashed random session tokens, and TLS 1.3. No OAuth or guest login in v1. | Default (from your direction to protect passwords with a real-world method) | Add OAuth or guests. |
| DEC-46 | The five initial games use the rule sets in section 22: draw conditions per game (DEC-48), no clock increment, the first mover alternates (DEC-58), leaving ends the game as a loss, hard-cap handling per game. | Default | Other variants and rules. |
| DEC-47 | v1 simplifications: xiangqi chase rules and shogi impasse declaration are not implemented. | Default | Implement them. |
| DEC-48 | Draw, repetition, move-limit and hard-cap rules belong to each game, in its own section and crate. No rules function is shared across games, and the match consumes only the game's result. Draw by agreement is allowed in all five games. | Confirmed (your answers) | Share rules helpers across games. |
| DEC-49 | Doushouqi's repetition handling is up to its own rules. v1 default: no repetition or move-limit draw, and clocks end long shuffling. | Open | Give doushouqi a repetition rule or a move limit. |
| DEC-50 | The match seed is drawn by the server at `StartMatch` and is never client-settable or visible before the match ends. | Default (security) | Host-chosen seeds in private test lobbies only. |
| DEC-51 | Reconnect sends a `Resync` message; `PublicLobby` entries have a fixed shape (PRO-14, PRO-15). | Default | Fold phase into `LobbyState`. |
| DEC-52 | An idle lobby is cancelled after 10 minutes (`LOBBY_IDLE_TIMEOUT`). | Default | Other timeout. |
| DEC-53 | A full engine event channel cancels the lobby instead of blocking (ARC-10). | Default | Drop events or apply backpressure. |
| DEC-54 | A player may be in at most one lobby at a time (SRV-11). | Default | Allow several lobbies. |
| DEC-55 | WebSocket tickets are sent only in the `Authorization` header, never in URLs (SEC-7). | Default | Subprotocol header. |
| DEC-56 | Inputs carry a server-stamped receipt time and `at_ms` is kept non-decreasing (TMR-1). | Default | Stamp at dequeue. |
| DEC-57 | RTT is measured with server-sent WebSocket ping frames; application `Ping`/`Pong` is for display only (LAT-6). | Default | Application-level timing pings. |
| DEC-58 | First mover: random for the first game that starts, then it alternates every game. | Confirmed | Random every game. |
| DEC-59 | Every MUST requirement needs a test, a CI check or a review entry; `spec/TRACEABILITY.md` is generated and checked in CI. | Confirmed | Manual matrix. |
| DEC-60 | Tied match totals share a rank; no tiebreaker. | Confirmed | Add a tiebreak rule. |
| DEC-61 | Players join only by matchmaking or by picking a lobby (public list or private code). Matchmaking has one preset (`quick`: 3 rounds of 360 s, 3 minutes per player), FIFO pairing and a 15 s accept step. Games are still drawn at random each round, so the same game can repeat. | Confirmed | More presets, ratings, or a no-immediate-repeat rule. |
| DEC-62 | The spec owns requirements and rules; the plan owns architecture, schema and shapes; the spec wins on conflict. | Confirmed | - |
| DEC-63 | Client UI: three-entry home, board-first match screen, the same game crates in WASM for move hints. | Default | Other UI structure. |
| DEC-64 | Reconnect grace: 30 s after a mid-round disconnect; the game and clocks keep running; the player may return and continue; after the grace they are out of that round. The grace starts when the server reports the disconnect, so a silent drop can take up to 45 s in total (15 s detection plus 30 s). | Confirmed (30 s, clock keeps running) | Other grace length, or pause the clocks. |
| DEC-65 | Accounts require an email and stay pending until the confirmation link is used. Registration and reset give generic responses, so emails cannot be enumerated; usernames are public. Reset goes only to active accounts. | Confirmed | Auto-login after confirming; verification optional. |
| DEC-66 | Signed-in players can change their password (current password required, other sessions signed out). Confirming an email does not sign the player in. | Confirmed | Reset by email only. |

## 19. Verification and change policy

Goal: use the lightest verification that matches what a change touched. Do not run every tool for every edit. This applies to every change, whether made by an AI or a human.

### 19.1 Default rule

- VER-1: Routine Rust edits rely on the editor hooks only.
- VER-2: The editor hooks run `cargo check`, `cargo clippy --fix` and `cargo test`. An AI MUST NOT run these commands locally. It MUST read the hook output and fix what the hooks report.
- VER-3: Every change summary MUST say, in one line each, whether Verus, fuzzing and benchmarks were run or skipped, and why.
- VER-4: Every crate MUST declare `#![forbid(unsafe_code)]`, so Miri is not needed.
- VER-5: Property tests (`proptest`, a dev-dependency) SHOULD cover scoring, ranking and presence invariants. They run inside the normal `cargo test` hook, so they cost nothing extra.

### 19.2 Decision rule

For each change, ask these questions in order:

| Question | If yes |
|---|---|
| Did the change touch verified logic (VER-12)? | Run `cargo verus verify -p superminigames-logic`. |
| Did the change touch an untrusted input path (VER-22)? | Run `cargo fuzz run <target> -- -max_total_time=60` for each affected target in VER-21. |
| Is the project feature-complete and is performance validation needed (VER-30)? | Run the Criterion benchmarks for the affected path (VER-31). |
| None of the above | Hooks only. |

### 19.3 Formal verification (Verus)

- VER-10: Verus verifies only the `superminigames-logic` crate. It opts in with `verify = true` under `[package.metadata.verus]` in its `Cargo.toml`. Other crates MUST NOT opt in unless a decision is added to section 18.
- VER-11: Verus supports a subset of Rust, so code in `logic` MUST stay plain and synchronous, using primitive integers, slices and `Vec`: no async, no `tokio`, no `serde`, no trait objects. Day to day, use `cargo verus verify -p superminigames-logic` (`cargo verus focus` MAY be used for quick iteration). CI MUST run the full `verify`.
- VER-12: Run Verus when a change touches: the `logic` crate; invariants; state-machine safety rules (section 12); timer or disqualification decisions; score computation (section 9).
- VER-13: The `logic` crate exposes these functions (names and semantics are normative; signatures are illustrative):

```rust
// superminigames-logic: primitive types only, no I/O
pub enum Phase { Lobby, Starting, InRound, RoundFinished, MatchFinished, Cancelled }
pub enum PhaseTrigger { /* one variant per trigger in the table of section 12.1 */ }
pub enum MissingOutcome { Keep, LeftRound, TimerExpired }

pub fn places_from_keys(keys: &[(u8, i128)]) -> Vec<(usize, usize)>; // (place, group)               // SCR-3, SCR-4
pub fn points_for_place(n: usize, place: usize, group: usize) -> u32;                   // SCR-6
pub fn round_points(keys: &[(u8, i128)]) -> Vec<u32>;                    // SCR-4, SCR-6, SCR-11
pub fn rank_players(removed: &[bool], totals: &[u32]) -> Vec<u32>;                           // FIN-1
pub fn next_streak(streak: u32, present: bool, k: u32) -> (u32, bool);    // (new streak, removed) PRS-2, PRS-3
pub fn next_phase(phase: Phase, trigger: PhaseTrigger) -> Option<Phase>; // section 12.1
pub fn fill_missing_outcome(has_result: bool, left: bool) -> MissingOutcome; // RND-9
pub fn add_points(total: u32, points: u32) -> u32;                        // SCR-10
```

- VER-14: The following properties MUST be proven. In verified functions, the `debug_assert!` preconditions of SCR-6 become `requires` clauses.

| Function | Spec | Properties to prove |
|---|---|---|
| `points_for_place(n, place, group)` | SCR-6, SCR-7, SCR-8 | Given 2 <= n <= 4, place >= 1, group >= 1 and place + group - 1 <= n: the result equals the average of the base points `12 * (n - q) / (n - 1)` over the places q = place..place+group-1, with no remainder; result <= 12; a group of 1 at place 1 gives 12; a group of 1 at place n gives 0; for groups of size 1 the result never increases as place grows. |
| `places_from_keys(keys)` | SCR-3, SCR-4 | Every place is in 1..=n; group >= 1 and place + group - 1 <= n; equal keys get equal (place, group); a strictly smaller key gets a strictly smaller place; place equals 1 plus the count of strictly smaller keys; group equals the count of equal keys; at least one player has place 1. |
| `round_points(keys)` | SCR-4, SCR-6, SCR-11 | The points sum to exactly `6 * n`; equal keys get equal points; a strictly smaller key never gets fewer points. |
| `rank_players(removed, totals)` | FIN-1 | A Removed player never gets a better rank than a player who was not Removed; within a group, equal totals get equal ranks and a higher total never gets a worse rank; each rank equals 1 plus the number of players that sort strictly before it. |
| `next_streak(streak, present, k)` | PRS-2, PRS-3 | Present resets the streak to 0; absent adds 1 without overflow; the player is removed exactly when the new streak is >= k. |
| `next_phase(phase, trigger)` | 12.1, ARC-5 | `Cancelled` and `MatchFinished` are absorbing; `Cancelled` is reachable only from `Lobby`; `InRound` is reachable only from `Starting`; every transition returned appears in the table of section 12.1. |
| `fill_missing_outcome(has_result, left)` | RND-9, PRS-6, TMR-4 | A present result is kept; a missing result with `left` gives `LeftRound`; a missing result without `left` gives `TimerExpired`. |
| `add_points(total, points)` | SCR-10 | With points <= 12 and at most 100 rounds, `total <= 12 * rounds_done` holds before and after, so no overflow can occur. |

- VER-15: Not provable by Verus, so covered by tests: selection uniformity (T-RNG-2), golden RNG values (T-RNG-1, T-RNG-6), the async timer loop (T-TMR-1 to T-TMR-4, run with paused time), the actor and serde code, and the rules of individual games.
- VER-16: Decision logic inside async code MUST be extracted into pure functions in `logic` so it can be verified. The timer loop calls `fill_missing_outcome` rather than inlining the rule.
- VER-17: **Proof failure policy.** A failing proof is blocking and MUST be resolved before merge. The AI MUST first inspect the failure and decide:
  1. If the implementation is wrong, fix the code.
  2. If the formal spec is wrong or underspecified (the proof is impossible as written), the AI MAY propose a spec change with the reason, but a human MUST approve it before merge.
  3. The AI MUST NOT force a proof through. It MUST NOT weaken or delete a `requires` or `ensures` clause, and MUST NOT add `assume`, `admit`, `#[verifier::external_body]`, `#[verifier::external]` or any equivalent escape hatch, unless a human-approved marker (`APPROVED-BY:`) is present.
- VER-18: Every `requires` and `ensures` clause in `logic` MUST carry a comment naming the requirement ID it comes from (for example `// SCR-6`). Technical clauses (overflow, bounds) cite the nearest requirement. A clause with no matching requirement in this document MUST NOT be added.
- VER-19: Staging: Verus is introduced in the hardening milestone (plan M8). Until then VER-12 to VER-18, T-VER-1, T-VER-2 and T-VER-4 are not enforced in CI, and the VER-14 properties are covered by `proptest`.

### 19.4 Fuzzing

- VER-20: Use `cargo fuzz run` only on untrusted-input boundaries, and always time-boxed: `cargo fuzz run <target> -- -max_total_time=<seconds>`. Defaults: 60 seconds per affected target per change; 10 minutes per target in nightly CI. Fuzz targets live in the `fuzz/` directory of the owning crate.
- VER-21: The AI MUST choose targets from this list and MUST NOT fuzz the whole repo blindly:

| Target | Crate | Boundary | Spec IDs | Invariants checked |
|---|---|---|---|---|
| `protocol_decode` | `protocol` | Binary (MessagePack) decode of `Envelope`, `ClientMessage`, `ServerMessage` | PRO-1, PRO-2, PRO-10 | Never panics; a decoded message re-encodes and decodes equal; oversize input is rejected. |
| `join_code_parse` | `core` | `JoinCode` constructor, config and filter validation | LOB-3, LOB-12 | Never panics; accepted codes are exactly 6 characters from A-Z and 0-9. |
| `engine_commands` | `engine` | Arbitrary sequences of `Command` from arbitrary players (join, leave, ready, settings, connection, input) | LOB-*, PRS-*, FLR-8 | Never panics; at most `MAX_PLAYERS` seats; every phase change is allowed by `next_phase`; totals stay within `12 * rounds`. |
| `game_action_<game>` | each game crate | `GameAction` payloads passed to `tick` | GAM-6, T-GAM-3 | Never panics; `result()` passes RND-9 validation. |
| `replay_log_decode` | `engine` | Decode and replay of a `ReplayLog` | REP-2 | Never panics; replay is deterministic. |
| `storage_load` | `server` | Loading a persisted `MatchReport` | STO-2 | Never panics; save then load round-trips. |
| `auth_input` | `server` | Username and password parsing and validation (length, Unicode normalization) | SEC-4, SEC-10 | Never panics; accepted values satisfy the policy; no input makes hashing allocate beyond its fixed budget. |

- VER-22: Run fuzzing when a change touches protocol decoding, message parsing, serialization or deserialization, lobby join/leave handling, game action handling, or any save/load format.
- VER-23: Fuzz harnesses MUST be deterministic and MUST NOT read the wall clock or OS randomness.
- VER-24: A crash, timeout or out-of-memory is blocking. Minimize the input, add it as a regression unit test (it then runs via the hook), commit it to the corpus, then fix the bug. Corpora are committed.

### 19.5 Benchmarks (Criterion)

- VER-30: Benchmarks are non-blocking and reserved for late-stage work. Run them only after the project is feature-complete, or when a late-stage performance concern appears. "Feature-complete" means all tests in section 17 are implemented and passing (Default).
- VER-31: Benchmark only performance-sensitive code: score aggregation (`places_from_keys`, `points_for_place`, `rank_players`); selection (pool building, `uniform_index`); round simulation with a scripted game; protocol encode and decode; the engine tick loop; server tick and latency-sensitive paths.
- VER-32: Run `cargo bench` in the owning crate and compare with a stored baseline. A regression is reported in the change summary and never blocks a merge.
- VER-33: Never run benchmarks for routine edits.

### 19.6 Policy checks

- T-VER-1: Given the `logic` crate, when CI scans it, then no `assume`, `admit`, `external_body` or `external` appears without an `APPROVED-BY:` comment on the same or previous line (VER-17).
- T-VER-2: Given a change that touches `logic`, then CI runs `cargo verus verify -p superminigames-logic` and the change cannot merge if it fails (VER-12, VER-17, VER-14).
- T-VER-3: Given the list in VER-21, then every target exists in the repo and builds, and CI runs each for its time budget (VER-20, VER-21).
- T-VER-4: Given any `requires` or `ensures` clause in `logic`, then it carries a requirement-ID comment (VER-18).
- T-VER-5: Given a change that only edits one game's rules, then only the hooks and that game's `game_action_<game>` fuzz target run; Verus and benchmarks are skipped (VER-1, VER-2, VER-22).
- T-VER-6: Given the workspace, then only `superminigames-logic` declares `[package.metadata.verus]` (VER-10).

### 19.7 Traceability

- VER-40: Every requirement that contains MUST has at least one verification: a test (a `T-*` item that cites its ID), a CI check (VER-41), or a review entry (VER-42).
- VER-41: CI checks:

| CI check | Rule | Covers |
|---|---|---|
| CI-1 | Dependency rules between crates (T-WSP-1) | WSP-1, WSP-2, WSP-6, PRO-4, STO-1, VER-11 |
| CI-2 | clippy `disallowed_methods` and `disallowed_types` in `engine`, `core`, `logic` and game crates: `SystemTime::now`, `Uuid::new_v4`, `thread_rng`, `OsRng`, `std::thread::spawn`, `HashMap`, `HashSet`; and `Instant::now` outside `engine` | RNG-7, RNG-8, GAM-2, TYP-3 |
| CI-3 | `clippy::float_arithmetic` denied in `logic` and the `core` scoring adapters | SCR-10 |
| CI-4 | Every crate root has `#![forbid(unsafe_code)]` | VER-4, SEC-16 |
| CI-5 | `tracing` feature `release_max_level_info` is enabled in release builds | PERF-10 |
| CI-6 | No `Mutex` or `RwLock` in `engine` | ARC-2, PERF-3 |
| CI-7 | Every game crate runs the conformance suite | GAM-11 |
| CI-8 | `spec/traceability.py --check` fails on an uncovered MUST requirement or a stale `spec/TRACEABILITY.md` | VER-40, VER-41, VER-42, VER-43 |

- VER-42: Review-only requirements (checked in code review, not by a test or CI): LAT-3, LAT-9, PERF-4, SCR-2, TMR-7, VER-3, VER-16, VER-23, WSP-8.
- VER-43: `spec/TRACEABILITY.md` is generated from this spec by `spec/traceability.py` and committed. It lists every MUST requirement with the tests, CI check or review entry that verify it. It is never edited by hand.

## 20. Latency and performance

### 20.1 Latency

- LAT-1: Goal: the lowest practical end-to-end move latency. Nothing on the input path may add artificial delay: no batching timers, no debounce, `TCP_NODELAY` on every game socket, and no compression of game frames (PRO-11).
- LAT-2: Server-side processing budget (Default): from the moment an `Input` is dequeued by the actor to the moment the resulting events are queued for sending, p99 <= 1 ms and p99.9 <= 5 ms at the target load (PERF-1). It is measured by the `tick_latency` histogram (OBS-3).
- LAT-3: The tick path MUST NOT perform storage access, password hashing, blocking I/O or unbounded allocation. Serialization for each subscriber happens on connection tasks, not on the actor.
- LAT-4: **Premoves.** No premove message exists (PRO-12). Premoves are a client-side queue:
  - The client MUST send a queued premove in the same event-loop turn in which it processes the opponent's move event, with no added delay.
  - The server validates a premove exactly like any move. A premove that has become illegal, or that arrives out of turn, is rejected with `InputRejected` and costs no clock time (GAM-13).
  - The client MUST clear its premove queue when any input is rejected and when the game ends.
  - The client MAY show moves optimistically; the server stays authoritative.
- LAT-5: Message size budgets (Default): a move input is at most 40 bytes and a typical move event at most 64 bytes, including the envelope (PRO-1).
- LAT-6: The server measures each participant's round-trip time with WebSocket ping control frames that the server sends every 1 second while a round is in progress (and at the 5-second heartbeat otherwise); browsers and native clients answer them automatically. The server keeps an exponentially weighted moving average per connection. RTT is measured only by the server. The application-level `Ping`/`Pong` messages exist only for a client's own latency display and are never used for lag credit. Client-reported latency or timestamps MUST NOT be trusted.
- LAT-7: For each `Input`, the server computes `lag_credit_ms = min(rtt_ewma_ms, LAG_CAP_MS)` and passes it in `Command::Input`. The engine enforces the per-round total cap (TMR-11). Games that keep per-player clocks charge `max(0, elapsed_ms - lag_credit_ms)` to the mover (CLK-2).
- LAT-8: Clocks are server-authoritative. The clock of the player to move starts at the `at_ms` of the tick that applied the opponent's previous move. Clients display clock values from server events and only count down locally between events.
- LAT-9: Multi-region deployment and region-aware matchmaking are out of scope for this version, but the protocol MUST NOT prevent adding a region field to the lobby later.

### 20.2 Performance

- PERF-1: Capacity target (Default): at least 50,000 concurrent live matches (100,000 connections) on one 8-core, 16 GiB instance while meeting LAT-2.
- PERF-2: Memory budget (Default): at most 100 KiB per live 2-player match in steady state, including the match actor and both connections' buffers, with replay recording off.
- PERF-3: Concurrency model: one async task per match and per connection on a multi-threaded tokio runtime. No OS thread per match or per connection, and no `Mutex` on the tick path (ARC-2).
- PERF-4: Compact state: games MUST use compact representations (for example bitboards or fixed arrays) and SHOULD NOT allocate per move on the tick path. Action and event payloads are small binary blobs (PRO-1), never JSON values.
- PERF-5: A live match keeps only the seat list, its round records (at most 100) and the current game instance. Replay logs are off by default (REP-3).
- PERF-6: Bounded buffers: a connection's read buffer starts at 4 KiB or less and never exceeds 64 KiB (PRO-10); outbound queues are bounded (SRV-7); the engine inbox is bounded (TMR-8).
- PERF-7: Admission control: when live matches or memory exceed configured limits, the server MUST refuse new lobbies with `ServerBusy` instead of degrading the latency of running matches.
- PERF-8: Password hashing runs on a dedicated bounded pool (SEC-3) so login bursts cannot starve match tasks or exhaust memory.
- PERF-9: Use one re-armed timer per match for the deadline and wake-ups (TMR-2), not one per tick or per player.
- PERF-10: `tracing` levels `trace` and `debug` MUST be compiled out of release builds (`release_max_level_info`), and filtered logs MUST NOT allocate to format messages.
- PERF-11: Per-game cost budget (Default): `tick` for a legal move p99 <= 100 microseconds on the reference machine for each of the five initial games. Checked with Criterion (VER-31), non-blocking (VER-32).

### 20.3 Checks

- T-LAT-1: Given an accepted game socket, then `TCP_NODELAY` is set and frames are sent without batching delay or compression (LAT-1, PRO-11).
- T-LAT-2: Given a typical move input and move event, then their encoded sizes meet LAT-5.
- T-LAT-3: Given a client with a queued premove, when the opponent's move event is processed, then the premove is sent in the same event-loop turn; and when the server rejects it, the queue is cleared and no clock time is charged (LAT-4, GAM-13).
- T-LAT-4: Given a client that sends its own latency or timestamp values, then the server ignores them and uses only its measured RTT (LAT-6).
- T-LAT-5: Given a running match, then the `tick_latency` histogram is recorded (LAT-2, OBS-3).
- T-PERF-1 (load test, nightly, reported not blocking): Given scripted clients playing 50,000 concurrent matches, then LAT-2 holds and memory per match meets PERF-2 (PERF-1, PERF-2).
- T-PERF-2: Given the server at its configured limit, then a new lobby is refused with `ServerBusy` and running matches keep meeting LAT-2 (PERF-7).
- T-PERF-3: Given a burst of logins, then match tick latency stays within LAT-2 (PERF-8, SEC-3).

## 21. Security and threat model

### 21.1 Assets and attackers

- Assets: account credentials, session tokens, match integrity (moves, clocks, results), availability, player privacy.
- Attackers: anonymous internet users; malicious players (cheating, griefing, abusing lag compensation); credential stuffers; denial-of-service actors; clients sending malformed or oversized data; someone holding a copy of the database; compromised dependencies.

### 21.2 Requirements

- SEC-1: **Transport.** All client-server traffic MUST use TLS 1.3. There is no plaintext listener except an HTTPS redirect. Web responses send HSTS.
- SEC-2: **Password storage.** Passwords MUST be hashed, never encrypted or stored reversibly. Use Argon2id with a unique random salt of at least 16 bytes per password, stored in PHC string format so parameters can be raised later. Parameters MUST be at or above the current OWASP Password Storage Cheat Sheet minimums and tuned on production hardware (Default starting point: 19 MiB memory, 2 iterations, 1 lane). On a successful login the server MUST rehash if the stored parameters are outdated. A server-side pepper held outside the database (secrets manager or KMS) is RECOMMENDED.
- SEC-3: **Hash isolation.** Hashing MUST run on a dedicated bounded blocking pool (Default 8 concurrent hashes). If the queue is full the request fails with `ServerBusy`. Logging in with an unknown username MUST perform an equivalent dummy hash so timing does not reveal which usernames exist.
- SEC-4: **Password policy.** Default: minimum 10 characters, maximum at least 64, any Unicode (normalized to NFKC), no composition rules, no forced rotation, and known-breached passwords are rejected (checked against a local list or a k-anonymity service).
- SEC-5: **Login throttling.** Failed logins are rate-limited per account and per IP with exponential backoff. Error messages are generic ("invalid credentials"). Account lockout MUST NOT let an attacker lock out other users for long; use backoff, not permanent lock.
- SEC-6: **Sessions.** Tokens are 256 random bits from the OS CSPRNG. Only a SHA-256 hash of the token is stored server-side. Tokens expire (Default: 30 days absolute), are rotated on login, and are revocable (including "log out everywhere"). Cookies use `HttpOnly; Secure; SameSite=Strict`. Tokens never appear in URLs or logs.
- SEC-7: **WebSocket authentication.** Browsers authenticate the upgrade request with the session cookie. Native clients send `Authorization: Bearer <ticket>` with a short-lived single-use ticket. Tickets MUST NOT be sent in URLs or query strings. The `Origin` header MUST be checked against an allow list. Unauthenticated sockets are rejected.
- SEC-8: **Authority.** The server is authoritative for all game state, clocks and scores (PRO-5). Clients never supply time. Lag credit comes only from server-measured RTT (LAT-6). Inputs from non-participants are rejected.
- SEC-9: **Abuse limits.** Enforce `LAG_CAP_MS` and `LAG_TOTAL_CAP_MS` (TMR-11); a per-connection input rate limit (Default 20 inputs per second); per-account limits on lobby creation and open lobbies; and limits on connections per IP and per account.
- SEC-10: **Input validation.** All untrusted input is validated at the server boundary (size, schema) with bounded parsing, and is covered by the fuzz targets in VER-21.
- SEC-11: **Injection.** Database access uses parameterized `sqlx` queries only, never SQL built from strings, with a least-privilege database user.
- SEC-12: **Secrets.** No secrets in the repository or logs. Secrets load from the environment or a secrets manager and are rotated on suspected compromise.
- SEC-13: **Supply chain.** `Cargo.lock` is committed, versions are pinned (SCP-3), and CI runs `cargo-deny` and `cargo-audit` to block known-vulnerable or disallowed crates.
- SEC-14: **Logging and privacy.** Logs never contain passwords or tokens, and IP addresses are truncated or hashed. Store only what is needed (username, email, password hash, creation time, match results). A user can delete their account (Default: credentials and email are deleted and match history is anonymized). Log retention is at most 30 days (Default).
- SEC-15: Security tokens use the OS CSPRNG, and only in the server (RNG-7, SRV-4).
- SEC-16: Memory safety: every crate declares `#![forbid(unsafe_code)]` (VER-4).
- SEC-17: **DoS resilience.** Bounded channels (TMR-8, SRV-7), connection and handshake timeouts (SRV-3), message size limits (PRO-10) and admission control (PERF-7).
- SEC-18: **Match integrity.** The match seed is drawn by the server at `StartMatch` and is never client-settable (SRV-10), so a host cannot choose which games are played or who moves first.
- SEC-19: **Registration.** Registration takes a username, an email and a password. `POST /register` MUST return the same generic `202` response whether or not the email is already registered; only `username_taken` may differ (usernames are public). A new email creates a pending account and gets a confirmation link; an email with a pending account gets the link again; an active account's email gets an "already registered" notice and nothing is created. The token is 256 random bits from the OS CSPRNG, stored only as SHA-256, valid 24 hours and single use. A pending account cannot log in and is deleted after 24 hours. Confirming does not sign the player in; they log in afterwards. `POST /email/resend { email }` returns the same `202` and re-sends only for a pending account. Email is stored lowercased and unique.
- SEC-20: **Password reset.** `POST /password/forgot { email }` MUST return the same response whether or not the address belongs to an active account, and MUST send mail asynchronously so timing does not reveal it. A reset token is 256 random bits from the OS CSPRNG, stored only as SHA-256, valid 30 minutes and single use; a new request invalidates older tokens. `POST /password/reset { token, new_password }` MUST apply the password policy (SEC-4), store a new Argon2id hash, revoke all sessions of the account, and send a "your password was changed" notice.
- SEC-21: **Mail limits.** Registration, resend and reset requests MUST be rate-limited per email address and per IP (Default: 3 per hour per address, 10 per hour per IP). Email addresses are never written to logs in full (truncate or hash) and are deleted with the account (SEC-14).
- SEC-22: **Change password.** `POST /password/change { current_password, new_password }` (signed in) MUST verify the current password on the hash pool (SEC-3), apply the password policy (SEC-4), store a new Argon2id hash, revoke every other session of the account (the current one stays), and send a "your password was changed" notice. It is throttled like login (SEC-5).

### 21.3 Checks

- T-SEC-1: Given two accounts with the same password, then their stored hashes differ (unique salts) and each is an Argon2id PHC string (SEC-2).
- T-SEC-2: Given a stored hash with outdated parameters, when the user logs in successfully, then it is rehashed with current parameters (SEC-2).
- T-SEC-3: Given a database dump, then it contains no plaintext or reversible passwords and no raw session tokens (SEC-2, SEC-6).
- T-SEC-4: Given a login for an unknown username and one for a known username with a wrong password, then both perform a hash and return the same generic error (SEC-3, SEC-5).
- T-SEC-5: Given repeated failed logins, then they are throttled with backoff per account and per IP (SEC-5).
- T-SEC-6: Given a WebSocket upgrade with a disallowed `Origin` or no valid session, then it is rejected (SEC-7).
- T-SEC-7: Given a client that sends scores, outcomes or timestamps, then the server ignores them (SEC-8, PRO-5).
- T-SEC-8: Given CI, then `cargo-deny` and `cargo-audit` pass (SEC-13).
- T-SEC-9: Given a WebSocket upgrade with a ticket in the query string, then it is rejected; given `Authorization: Bearer` with a valid ticket, then it is accepted once and a second use is rejected (SEC-7).
- T-SEC-10: Given a TLS 1.2 client hello, then the handshake fails; given plain HTTP, then the server only redirects to HTTPS (SEC-1).
- T-SEC-11: Given `forgot` for an active account's address and for an unknown address, then both return the same status and body, and mail is sent only for the active account (SEC-20).
- T-SEC-12: Given a reset token, then it works once, expires after 30 minutes (paused time), is stored only as a hash, a newer request invalidates the older token, and a successful reset revokes all sessions and sends the change notice (SEC-20).
- T-SEC-13: Given more than 3 reset requests in an hour for one address, or more than 10 from one IP, then `RateLimited` (SEC-21).
- T-SEC-14: Given `register` with a free username for a new email and for an already registered email, then both return the same `202` body; the new email gets a confirmation mail and a pending account, the registered email gets an "already registered" notice and nothing is created; a taken username gives `username_taken` (SEC-19).
- T-SEC-15: Given a pending account, then login with the correct password gives `email_not_verified` and with a wrong password `invalid_credentials`; the confirmation token works once within 24 hours and does not create a session; `resend` re-sends only for a pending account; a pending account is deleted after 24 hours (SEC-19).
- T-SEC-16: Given change password with a wrong current password, then `invalid_credentials`; with the correct one, then a policy-compliant new hash is stored, every other session is revoked, the current session stays, and the notice is sent (SEC-22).

## 22. Initial mini-games

The rules in this section are the Default rule set for v1. Each game lives in its own crate and owns its rules (WSP-9): the match consumes only the game's result. Changes belong in section 18.

### 22.1 What the match needs from a game

- GME-1: Crates and ids: `superminigames-games/chess` (`GameId("chess")`), `checkers` (`"checkers"`), `xiangqi` (`"xiangqi"`), `shogi` (`"shogi"`) and `doushouqi` (`"doushouqi"`). Each exposes `pub fn create() -> Box<dyn Game>` (WSP-4).
- GME-2: All five support exactly 2 players (`supports_players(n)` is `n == 2`), use `ScoringMode::Placement` and `TimeModel::PlayerClocks`, and are perfect-information games (no `ToPlayer` events).
- GME-3: The first player in `players` moves first (White in chess, Black in checkers, Red in xiangqi, Sente in shogi, the first side in doushouqi). A game never chooses the first mover itself.
- GME-4: Results: a win gives the winner `Placed(1)` and the loser `Placed(2)`. A draw, for games that define one, gives both `Placed(1)`, which splits the points (SCR-6). Ways to lose: checkmate or no legal move (per game), resignation, clock reaching zero, abandonment.
- GME-5: Each game defines its own action encoding in its own section (22.3 to 22.7). The engine never interprets action bytes. A malformed or unknown action is rejected with `InputRejected` (GAM-6).
- GME-6: **Draw by agreement** (Confirmed: allowed in all five games) is implemented by each game itself. A player may have one outstanding offer. It lapses when the opponent moves or declines. Accepting with no outstanding offer from the opponent is rejected. An accepted offer ends the game as a draw.
- GME-7: Clocks follow section 22.2.
- GME-8: **Leaving.** On `PlayerLeft` (sent after the reconnect grace, PRS-6) the game ends immediately: the leaver is `NoResult(LeftRound)` and the opponent is `Placed(1)`. (If both players leave, the engine ends the round, RND-13.)
- GME-9: **Hard cap.** If `result()` is called while the game is unfinished (the round deadline was reached), the game applies its own hard-cap rule, stated in its section. It is a safety net, not a rules draw: with no clock increment (CLK-1) it should not occur in normal play.
- GME-10: Each game defines its own event payloads; the engine never interprets them. The initial games SHOULD emit, after every accepted action, one `Broadcast` containing the applied action bytes, both remaining clocks in milliseconds (u32 little-endian each) and a state byte (0 playing, 1 first mover won, 2 second mover won, 3 draw), so clients can render without extra requests.
- GME-11: Each game MUST ship rule-conformance tests with published reference data (perft node counts for standard chess positions; hand-verified position suites for the others).
- GME-12: **Game-owned termination.** Everything about how a game decides legality, termination, draws, repetition, move limits and its hard-cap result belongs to that game (WSP-9). A game MUST NOT end in a draw by a condition its own section does not list, and MUST NOT borrow another game's rules.

### 22.2 Clocks

Default clock rule for the initial games. Each game implements it inside its own crate (WSP-9), and a game MAY use different clock rules.

- CLK-1: Each player starts with `time_limit_ms / 2` of clock time and there is no increment (Default). The two clocks together never exceed the round deadline.
- CLK-2: The clock of the player to move starts at the `at_ms` of the tick that applied the opponent's previous move (`turn_start_ms`); for the first mover it starts at `at_ms = 0`. When a move arrives at `at_ms` with `lag_credit_ms`, the mover is charged `max(0, at_ms - turn_start_ms - lag_credit_ms)`, never a negative amount. If the charge exceeds their remaining time, they lose on time and the move is not applied.
- CLK-3: `next_wakeup_ms` returns `turn_start_ms + remaining_ms + CLOCK_GRACE_MS` for the player to move, where `CLOCK_GRACE_MS = LAG_CAP_MS` (Default). When the engine ticks `TimeAdvanced` at or after that time, the player to move loses on time. The grace lets a legitimate move still in flight arrive and be charged correctly before the flag falls.
- CLK-4: Chess exception: if the player who ran out of time could never be checkmated (the opponent has insufficient mating material), the game is a draw instead.
- CLK-5: Clocks are server-authoritative (LAT-8).

### 22.3 Chess

- CHS-1: Standard chess rules: castling, en passant, promotion, check, checkmate. Stalemate is a draw. Insufficient material is a draw (king vs king, king and one minor piece vs king, king and bishop vs king and bishop with bishops on same-colour squares).
- CHS-2: Automatic draws (Default): threefold repetition (same position, side to move, castling rights and en passant possibility), and 50 moves (100 plies) without a pawn move or capture.
- CHS-3: Squares are numbered 0 to 63 with a1 = 0, b1 = 1, ..., h8 = 63. Move payload: `[0, from, to, promotion]` where promotion is 0 none, 1 knight, 2 bishop, 3 rook, 4 queen. Castling is a king move of two squares. The first mover plays White.
- CHS-4: Other actions: Resign `[1]`, OfferDraw `[2]`, AcceptDraw `[3]`, DeclineDraw `[4]`. Draw by agreement is allowed (GME-6).
- CHS-5: Hard cap (this game's own rule): a game still unfinished at the round deadline is a draw.

### 22.4 Checkers (English draughts)

- CHK-1: English draughts on 8x8, played on the 32 dark squares. Men move one square diagonally forward; kings move one square diagonally in any direction. Capture is mandatory. A multiple jump must be completed. A man reaching the far row becomes a king and the move ends. A player with no legal move loses.
- CHK-2: Automatic draw (this game's own rule, Default): 40 moves (80 plies) without a capture or a man move. Checkers has no repetition draw.
- CHK-3: Dark squares are numbered 0 to 31, row by row, starting from the first mover's back row. Move payload: `[0, n, sq_1, ..., sq_n]` with n >= 2: the start square followed by every landing square. The first mover plays Black, the side that moves first in English draughts.
- CHK-4: Other actions: Resign `[1]`, OfferDraw `[2]`, AcceptDraw `[3]`, DeclineDraw `[4]`. Draw by agreement is allowed (GME-6).
- CHK-5: Hard cap (this game's own rule): a game still unfinished at the round deadline is a draw.

### 22.5 Xiangqi

- XQ-1: Standard xiangqi on 9 files by 10 ranks: general, advisors, elephants, horses (blockable at the leg), chariots, cannons (capture by jumping exactly one screen), soldiers. Elephants cannot cross the river. Generals and advisors stay in the palace. The two generals may never face each other on an open file. Checkmate and stalemate both lose for the player to move.
- XQ-2: Repetition and move limit (Default, simplified for v1): a move that gives check and would create a third repetition by continuous checking is illegal. Otherwise threefold repetition is a draw. 60 moves (120 plies) without a capture is a draw. Chase rules are not implemented in v1 (DEC-47).
- XQ-3: Squares are numbered `rank * 9 + file`, 0 to 89, with rank 0 the first mover's back rank and file 0 on that player's left. Move payload: `[0, from, to]`. The first mover plays Red.
- XQ-4: Other actions: Resign `[1]`, OfferDraw `[2]`, AcceptDraw `[3]`, DeclineDraw `[4]`. Draw by agreement is allowed (GME-6).
- XQ-5: Hard cap (this game's own rule): a game still unfinished at the round deadline is a draw.

### 22.6 Shogi

- SHG-1: Standard shogi on 9x9 with drops. A captured piece goes to the capturer's hand and may later be dropped on an empty square instead of making a move. The promotion zone is the three far ranks; promotion is optional unless the piece would have no legal move (pawn or lance on the last rank, knight on the last two ranks). Illegal: a second unpromoted pawn in the same file for the same player (nifu), a pawn drop that gives checkmate (uchifuzume), and a drop where the piece could never move. Checkmate loses, and a player with no legal move loses.
- SHG-2: Repetition: the same position four times (sennichite) is a draw, except that the side that gave check on every move of the repetition loses. Impasse declaration (jishogi) is not implemented in v1 (DEC-47).
- SHG-3: Squares are numbered `rank * 9 + file`, 0 to 80, from the first mover's view. Move payload: `[0, from, to, promote, drop]` where `from` is 0xFF for a drop, `promote` is 0 or 1, and `drop` is 0 for a board move or the piece kind for a drop (1 pawn, 2 lance, 3 knight, 4 silver, 5 gold, 6 bishop, 7 rook). The first mover plays Sente (Black).
- SHG-4: Other actions: Resign `[1]`, OfferDraw `[2]`, AcceptDraw `[3]`, DeclineDraw `[4]`. Draw by agreement is allowed (GME-6).
- SHG-5: Hard cap (this game's own rule): a game still unfinished at the round deadline is a draw.

### 22.7 Doushouqi (Jungle)

- DSQ-1: 7 columns by 9 rows. Each side has 8 animals ranked: elephant 8, lion 7, tiger 6, leopard 5, wolf 4, dog 3, cat 2, rat 1. A piece captures an enemy piece of equal or lower rank, except that the rat may capture the elephant and the elephant may not capture the rat.
- DSQ-2: The board has two river areas, each 2 columns by 3 rows, in the middle rows. Only rats may enter water. A rat in water cannot capture a piece on land and cannot be captured by one. The lion and the tiger may jump over a river in a straight line (horizontally or vertically), landing on the first land square beyond, unless any rat occupies a water square on the jumped path. They may capture on landing if rank allows.
- DSQ-3: Each side has a den at the centre of its back row and three trap squares around it. A piece standing in an opponent's trap has rank 0 and any enemy piece can capture it. A piece may not enter its own den. Moving into the opponent's den wins. Capturing all enemy pieces wins. A player with no legal move loses.
- DSQ-4: Draws (this game's own rule, Default): by agreement only. Doushouqi has no repetition or move-limit draw in v1; the clocks end long shuffling. Whether to add a repetition rule is up to the doushouqi rules and is open (DEC-49).
- DSQ-5: Squares are numbered `row * 7 + col`, 0 to 62, from the first mover's view. Move payload: `[0, from, to]`.
- DSQ-6: Other actions: Resign `[1]`, OfferDraw `[2]`, AcceptDraw `[3]`, DeclineDraw `[4]`. Draw by agreement is allowed (GME-6).
- DSQ-7: Hard cap (this game's own rule): the player with the higher total rank of remaining pieces wins; if the totals are equal, the game is a draw.

### 22.8 Checks

- T-GME-1: Given each game, then Resign, OfferDraw, AcceptDraw and DeclineDraw behave per GME-4 to GME-6 and that game's own section, and malformed payloads are rejected (GME-5).
- T-GME-2: Given each game and `time_limit_ms = 600_000`, then each player's clock starts at 300_000 and the first mover's clock starts at `at_ms = 0` (CLK-1, CLK-2).
- T-GME-3: Given elapsed 1500 ms and lag credit 1000 ms, then the mover is charged 500 ms. Given elapsed 300 ms and credit 1000 ms, then the charge is 0 (CLK-2).
- T-GME-4: Given the mover's clock reaches zero and `TimeAdvanced` is ticked at `next_wakeup_ms`, then the opponent wins and the loser is `Placed(2)`. A legitimate move arriving before that wake-up is processed normally. In chess, if the opponent has insufficient mating material, the game is a draw (CLK-3, CLK-4, GAM-12).
- T-GME-5: Given `PlayerLeft`, then the game finishes immediately with the leaver `NoResult(LeftRound)` and the opponent `Placed(1)` (GME-8).
- T-GME-6: Given an unfinished game when `result()` is called at the hard cap, then the result follows that game's own hard-cap rule: a draw for chess, checkers, xiangqi and shogi (CHS-5, CHK-5, XQ-5, SHG-5); the remaining-piece rank total for doushouqi, with a draw only if equal (DSQ-7) (GME-9).
- T-GME-7: Given a move that is illegal or sent out of turn, then `InputRejected` and no clock change (GAM-13).
- T-GME-8: Given players passed as `[x, y]`, then `x` moves first in every game (GME-3, RND-11).
- T-GME-9: Given the reference rule suites, then every game passes them (GME-11).
- T-GME-10: Given the same seed, players and `(at_ms, input)` sequence, then replaying a game reproduces the same events and result for every game (REP-2).
- T-GME-11: Given each game, then it ends in a draw only by the conditions listed in its own section; in particular, repeated positions do not end a checkers or doushouqi game in a draw (GME-12).
- T-WSP-2: Given the game crates, then none depends on another game crate, and `core` contains no rules, draw or clock code (WSP-9, WSP-1, WSP-5).

## 23. Joining a match and client UI

### 23.1 Joining

- JOIN-1: There are exactly two ways to join a match (Confirmed): matchmaking ("Play now"), or picking a lobby (from the public list, or by entering a private lobby's code). v1 has no invite links and no friend lists.
- MAT-1: Matchmaking has one preset for now (Confirmed): `quick` = 3 rounds of 360 s (each player's chess clock is 180 s), an empty ban list (all games) and `max_absent_round_starts = 3`. More presets MAY be added later; `JoinQueue` carries the preset.
- MAT-2: `JoinQueue { preset }` puts the player in that preset's FIFO queue. When 2 players are waiting in the same preset, the server pairs the two oldest, creates a private lobby with the preset's config (its code is generated by the server and never shown to players), and seats both. There are no ratings in v1; players in different presets never pair.
- MAT-3: The server sends `MatchFound { lobby_id, accept_secs }` to both (`MATCH_ACCEPT_TIMEOUT = 15` s). Each player accepts with `SetReady { ready: true }`. When both are Ready, the server sends `StartMatch` on the host's behalf. If a player does not accept in time or leaves, the lobby is cancelled and the other player returns to the front of the queue.
- MAT-4: A queued player counts as being in a lobby (SRV-11): they cannot create or join a lobby until they send `LeaveQueue`. Disconnecting removes a player from the queue.
- MAT-5: Anti-dodge (Default): 3 missed accepts within 10 minutes block queueing for 5 minutes (`RateLimited`).
- MAT-6: `QueueStatus { state, waited_secs }` is sent when a player joins the queue, every 5 s while waiting, and when the queue is left.
- MAT-7: Matchmade lobbies are fixed. The server MUST reject `UpdateSettings`, `CancelLobby` and `JoinLobby` from clients for them (`WrongState`); a player can only accept (`SetReady`) or leave.

### 23.2 Client UI

Decision: a three-entry home screen and a board-first match screen. Players spend almost all their time in the match screen, so clarity and latency matter there; joining should take one tap.

- UIX-1: The home screen MUST offer exactly these entries: Play now (joins the queue), Browse lobbies, Create lobby.
- UIX-2: The lobby list SHOULD show host, seats, rounds and time per round, refresh every 3 s while open (rate-limited, SEC-9), and have a "Join by code" field for private lobbies.
- UIX-3: The lobby room SHOULD show both seats with ready state, a Ready button and Leave; the host also sees settings (disabled while anyone is Ready, LOB-6) and Start.
- UIX-4: The match screen SHOULD fill the screen with the board, both clocks beside it (server values, counted down locally), a thin strip with "Round n of N" and running totals, Resign and Offer draw in a menu, and a connection indicator (RTT from `Ping`).
- UIX-5: Premove MUST work as follows: when it is not the player's turn, a move can be queued; it is highlighted; one gesture cancels it; it is sent immediately when the opponent's move arrives (LAT-4); and it is cleared when rejected.
- UIX-6: Round and match results SHOULD appear as a short overlay with places and points, then auto-continue with a visible countdown; the final screen shows ranks and "Back to home".
- UIX-7: On reconnect the client SHOULD show a banner and use `Resync` (PRO-14). While the opponent is `Away` mid-round it SHOULD show a countdown of the reconnect grace (`RECONNECT_GRACE_SECS`, exported by `protocol`). If the player is not in the current round it SHOULD say "You will rejoin next round".
- UIX-8: Layout SHOULD be portrait-first on phones and the same layout scaled on desktop; touch targets at least 44 px; highlights not relying on colour alone; playable by keyboard; a reduced-motion setting.
- UIX-9: The web client MUST NOT keep a second copy of the game rules. It uses the same game crates compiled to WASM for legal-move hints and premove checks, and the server stays authoritative (game crates have no tokio, WSP-9).
- UIX-10: Auth screens SHOULD be Login, Register (then "check your email"), Confirm email, Forgot password, Reset password and Change password (in settings). The home screen is reachable only when signed in.

### 23.3 Checks

- T-MAT-1: Given two players in the `quick` queue, then they are paired in FIFO order, a private lobby with the preset's config is created, and both receive `MatchFound` (MAT-1, MAT-2).
- T-MAT-2: Given both players press Ready within the accept window, then the server sends `StartMatch` on the host's behalf and round 0 starts (MAT-3).
- T-MAT-3: Given one player does not accept in time, then the lobby is cancelled and the other player returns to the front of the queue (MAT-3).
- T-MAT-4: Given a queued player, when they send `CreateLobby` or `JoinLobby`, then `AlreadyInLobby`; given a queued player disconnects, then they leave the queue (MAT-4).
- T-MAT-5: Given a test build with a second preset registered, then players in different presets are never paired (MAT-2).
- T-MAT-6: Given 3 missed accepts within 10 minutes, then queueing is blocked for 5 minutes with `RateLimited` (MAT-5).
- T-MAT-7: Given a waiting player, then `QueueStatus` is sent on joining, every 5 s while waiting, and on leaving (MAT-6).
- T-MAT-8: Given a matchmade lobby, when a client sends `UpdateSettings`, `CancelLobby` or `JoinLobby` for it, then `WrongState`; accepting and leaving still work (MAT-7).
- T-UIX-1: Given the home screen, then it offers exactly Play now, Browse lobbies and Create lobby, and no other way to join (UIX-1, JOIN-1).
- T-UIX-2: Given a client with a queued premove, then it is highlighted, can be cancelled with one gesture, is sent when the opponent's move arrives, and is cleared on rejection (UIX-5, LAT-4).
- T-UIX-3: Given the web client, then it contains no game-rule code of its own and uses the game crates compiled to WASM (UIX-9).
