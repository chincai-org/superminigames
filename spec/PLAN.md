# SuperMiniGames Implementation Plan

Derived from `spec/SPEC.md` v0.1.

**Ownership:** the spec owns requirements, constants, message names and semantics, and game rules. This plan owns architecture, database schema, HTTP and WebSocket field shapes, tech stack, folder layout and milestones. If they conflict, the spec wins and this plan is corrected. Nothing from the spec is restated here; IDs point to it. IDs such as `LAT-2` or `SCR-6` refer to requirements in that spec.

Labels used in this plan:
- **Plan decision**: a choice the spec left open (mostly DEC-31). You may change it.

## 0. Summary

SuperMiniGames is a server-authoritative, 2-player match service. Each match is a series of rounds; each round runs one randomly chosen board game (chess, checkers, xiangqi, shogi, doushouqi). Players earn rank-based points per round and the totals decide the match.

The plan is one Rust workspace, one server process, one PostgreSQL database, and web, desktop and mobile clients that share one binary protocol.

| Area | Plan decision |
|---|---|
| Process model | One tokio actor per lobby and its match; one task per connection; games linked into the server binary |
| Transport | WebSocket over TLS 1.3 (rustls, in-process), MessagePack frames; HTTPS + JSON for accounts |
| Database | PostgreSQL via `sqlx` (results and accounts only; never live match state) |
| Metrics | `tracing` logs plus Prometheus metrics (spec left the format open, OBS-4) |
| Web client | TypeScript UI plus the `client` crate compiled to WASM |
| Desktop and mobile | Tauri 2 wrapping the same web UI |
| Verification | Editor hooks for routine checks; Verus, fuzzing and Criterion by trigger (section 19 of the spec) |

## 1. Architecture

### 1.1 Components and how they connect

```text
 Clients: browser / desktop / mobile (TypeScript UI + WASM `client` crate)
      |  HTTPS + JSON : register, login, logout, ws-ticket, history
      |  WSS + MessagePack : lobby and game protocol
      v
 +----------------------------------------------------------------------+
 | superminigames-server (one process)                                  |
 |                                                                      |
 |  rustls (TLS 1.3) -> axum router                                     |
 |    |-- http::auth ----> auth service --> bounded hash pool (Argon2id)|
 |    |-- http::history -> Storage trait --> PostgreSQL                 |
 |    '-- ws::gateway --> connection tasks (read loop + write loop)     |
 |                           | decode, validate, rate-limit, RTT       |
 |                           v                                         |
 |                    LobbyHost (1 per lobby)                           |
 |                      | Command (bounded mpsc)   ^ EngineEvent        |
 |                      v                          |                    |
 |                    Engine actor (1 per lobby)  --'                   |
 |                      | calls                                         |
 |                      v                                               |
 |       GameRegistry -> game crates: chess, checkers, xiangqi,         |
 |                                    shogi, doushouqi                  |
 |                                                                      |
 |  LobbyDirectory + Matchmaker   Storage writer (async, retry)         |
 |  Metrics (/metrics, internal listener)                               |
 +----------------------------------------------------------------------+
```

Library crates used by the engine: `core` (the `Game` trait, ids, config, RNG helpers) and `logic` (pure, verified decision functions).

### 1.2 Crates

Crate roles and dependency rules are in spec section 1.1. At runtime: `server` hosts one `engine` actor per lobby; `engine` calls games through the `Game` trait; `server` and `client` share `protocol`; the web client also loads the game crates as WASM for move hints (UIX-9).

### 1.3 Runtime model

- **Engine actor** (one tokio task per lobby): owns all lobby and match state (ARC-2). Receives `(Command, oneshot reply)` over a bounded `mpsc` (`INBOX_CAPACITY = 256`). Emits `EngineEvent`s over a bounded `mpsc` using `try_send`. A lobby idle for `LOBBY_IDLE_TIMEOUT` (10 minutes) is cancelled by the actor (LOB-16).
- **LobbyHost** (one task per lobby, server side): owns the engine handle, subscribes to its events, maps them to `ServerMessage`s, encodes each message once, and clones the encoded `Bytes` to the two connections' outbound queues. Encoding therefore never runs on the actor (LAT-3).
- **Connection task** (per socket): a read loop (decode, validate, rate-limit, build a `Command`) and a write loop (bounded outbound queue, default 64 frames). If the queue is full the connection is closed (SRV-7). The connection owns an RTT estimate in an `AtomicU32`, read without locks when building `Command::Input`.
- **Hash pool**: a dedicated bounded blocking pool (default 8 concurrent Argon2id hashes) so logins cannot starve match tasks (SEC-3, PERF-8).
- **Storage writer**: a background task that receives `MatchReport`s after `MatchFinished` and writes them with retry and backoff. The actor never waits on storage (STO-6).
- **Email sender**: sends verification, reset and password-changed mail from a bounded queue through an `EmailSender` trait (SMTP via `lettre`); the request path never waits for SMTP (SEC-20).
- **Timers**: one re-armed timer per match (PERF-9), covering the hard deadline and the game's wake-up (TMR-2).

Release profile: `lto = "fat"`, `codegen-units = 1`, `opt-level = 3`, and `panic = "unwind"`. Unwinding is required because the engine wraps every game call in `catch_unwind` (FLR-7); `panic = "abort"` would break that.

### 1.4 Key flows

**A. Sign in and connect**
1. Client `POST /api/v1/login`. The server verifies the Argon2id hash on the hash pool and creates a session (token returned in an `HttpOnly` cookie; only its SHA-256 is stored).
2. Client opens `GET /ws`. The server checks `Origin` and the cookie (or a bearer ticket for native clients), resolves a `PlayerId`, and spawns the connection tasks.
3. If the player has a live lobby (index `PlayerId -> LobbyId`), the connection is re-attached to it and the engine receives `Connection { connected: true }`.

**B. Lobby to match start**
1. `CreateLobby` creates a `LobbyId`, an `EngineInit` and an engine actor plus its LobbyHost. The host is seated.
2. A second player sends `JoinLobby` (with the code for private lobbies). Players send `SetReady`. The host sends `StartMatch`.
3. At `StartMatch` the server draws the match seed from the OS CSPRNG (SRV-10) and passes it with the command; the engine validates config and enters `Starting` for slot 0.

**C. Round start** (all inside the actor)
1. Presence snapshot, streak update, removal (PRS-2, PRS-3); end-of-match check (RND-2); participant check (RND-3).
2. Build the pool, derive the pick and `game_seed` from the keyed ChaCha8 streams (PLL-1, RNG-3, RNG-10).
3. Up to 4 attempts: fresh instance from the factory, `initialize(game_seed, players, time_limit_ms)`.
4. Phase `InRound`; emit `RoundStarting`.

**D. A move (latency path)**
```text
client frame -> TLS decrypt -> WS frame parse -> MessagePack decode
  -> Command::Input { player, action, lag_credit_ms, received_at }  (try_send to inbox)
  -> actor: tick(at_ms, GameInput::Player)  -> events
  -> EngineEvent (try_send) -> LobbyHost: encode once
  -> clone Bytes to both outbound queues -> TLS encrypt -> socket write
```
Targets: `tick_latency` p99 <= 1 ms (LAT-2). Plan metric `server_turnaround` (frame read to frame written): p99 <= 3 ms (plan default, extra to the spec).

**E. Disconnect and reconnect**
1. The connection task detects loss (no frame for 15 s, SRV-3) or a close, and sends `Command::Connection { connected: false }`.
2. Mid-round: the engine emits `PlayerStatus::Away` and starts the 30 s reconnect grace (PRS-6); the clocks keep running. If the player reconnects in time they continue the same round.
3. On reconnect the server sends `LobbyState`, `Resync`, `ScoreUpdate` and `PlayerStatus` (PRO-14). Within the grace the player continues the round; if the grace expires the engine ticks `PlayerLeft`, the game ends with the leaver as `NoResult(LeftRound)` (GME-8), and the player is a participant again at the next round start (PRS-7).

**F. Match end**
1. Engine emits `MatchFinished(MatchReport)`. The LobbyHost sends `MatchFinished` to clients and hands the report to the storage writer.
2. After persistence the actor and host tasks are dropped and the `LobbyDirectory` entry is removed.

### 1.5 Latency design

Requirements are in spec section 20. Plan-specific choices:
- Fan-out encodes each message once and shares the `Bytes` between both connections; the actor never serializes.
- Each connection keeps its RTT estimate in an `AtomicU32`, read without locks when building `Command::Input`.
- Connection tasks use `try_send` to the engine inbox and never await it.
- Extra metric `server_turnaround` (frame read to frame written), p99 <= 3 ms (plan default), next to the spec's `tick_latency`.

### 1.6 Memory and capacity plan

Targets are PERF-1 and PERF-2.

| Item per match | Estimate (to be measured in milestone M4) |
|---|---|
| Engine state, config, round records | 2-4 KiB |
| Game instance (bitboards, repetition history) | 1-4 KiB |
| LobbyHost state and channel blocks | 2-4 KiB |
| Two connections: TLS state, WebSocket state, buffers | dominant cost; estimate 20-40 KiB each |
| Kernel socket buffers (outside process RSS, but real RAM) | tune `tcp_rmem` and `tcp_wmem` |

**Risk:** TLS and socket buffers may exceed the 100 KiB budget. Mitigations, in order: lower per-connection buffer sizes and `rustls` buffer limits; pool buffers; terminate TLS at an edge proxy on the same host or a separate tier. Decide with measurements from M4, not guesses.

Admission control: `SMG_MAX_LIVE_MATCHES` refuses new lobbies with `ServerBusy` instead of degrading running matches (PERF-7).

### 1.7 Failure handling and isolation

| Failure | Handling |
|---|---|
| Game panics or returns `Fatal` | Caught per call; round `Skipped(GameAborted)`; match continues (FLR-4, FLR-7) |
| Game `initialize` fails | Retry up to 4 attempts, then `Skipped(StartFailed)` (FLR-1, FLR-3) |
| Actor or LobbyHost task panics | Isolated by tokio; the supervisor closes that lobby's connections and records a metric. Other lobbies are unaffected |
| Engine event channel full | `try_send` fails: treat the lobby as overloaded and tear it down (ARC-10) |
| Slow client | Outbound queue full: disconnect that client (SRV-7) |
| Database down | Matches keep running; reports queue for retry; `/readyz` reports not ready so no new lobbies start if the queue is over a limit (STO-6) |
| Server restart | Live matches are lost, not resumed (SRV-8, DEC-29). Deploy with a drain (section 8) |

### 1.8 Scaling path (after v1)

Single instance first. To scale out: route by `lobby_id` (consistent hashing at the load balancer, sticky WebSocket); move sessions, tickets and rate limits from process memory to the database or a shared store; keep the engine local to its lobby. Multi-region is out of scope but nothing in the protocol blocks it (LAT-9).

## 2. Data model

### 2.1 Domain types

Defined only in spec section 3. Engine state is never serialized to clients (PRO-4).

### 2.2 In-memory server structures

| Structure | Contents | Lifetime |
|---|---|---|
| `LobbyDirectory` | `LobbyId -> { handle, summary, is_public }`; secondary index `PlayerId -> LobbyId` | until lobby ends |
| `MatchmakingQueues` | per-preset FIFO of waiting `PlayerId`s with enqueue time; pairs waiting for accept | until paired or left |
| `TicketStore` | SHA-256(ticket) -> `{ player_id, expires_at }`, single use, TTL 30 s | in memory (move to a shared store when scaling out) |
| `RateLimiters` | token buckets keyed by IP, account, connection | sliding; evicted on idle |
| `Presence/RTT` | per-connection `AtomicU32` RTT EWMA | per connection |
| `GameRegistry` | prototypes and factories, sorted by `GameId` | process lifetime, immutable |

An account may be in at most one lobby at a time (SRV-11). This simplifies reconnect routing and limits abuse.

### 2.3 PostgreSQL schema (plan decision: PostgreSQL 16+)

Migrations live in `migrations/` and are applied by `sqlx migrate`. `u64` values (seeds) are stored as `bigint` using the two's-complement bit pattern (`as i64`).

```sql
-- 0001_accounts.sql
CREATE EXTENSION IF NOT EXISTS citext;

CREATE TABLE accounts (
  id            uuid PRIMARY KEY,
  username      citext NOT NULL UNIQUE
                CHECK (username ~ '^[A-Za-z0-9_-]{3,20}$'),
  password_hash text NOT NULL,                -- Argon2id PHC string, never plaintext
  email         citext NOT NULL UNIQUE,
  email_verified_at timestamptz,               -- NULL = pending, deleted after 24 h
  created_at    timestamptz NOT NULL DEFAULT now(),
  deleted_at    timestamptz
);

CREATE TABLE sessions (
  id           uuid PRIMARY KEY,
  account_id   uuid NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  token_hash   bytea NOT NULL UNIQUE,         -- SHA-256 of the 256-bit token
  created_at   timestamptz NOT NULL DEFAULT now(),
  last_seen_at timestamptz NOT NULL DEFAULT now(),
  expires_at   timestamptz NOT NULL,          -- default now() + 30 days
  revoked_at   timestamptz
);
CREATE INDEX sessions_account_idx ON sessions (account_id);

-- 0002_email_tokens.sql
CREATE TABLE email_tokens (
  id         uuid PRIMARY KEY,
  account_id uuid NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  purpose    text NOT NULL CHECK (purpose IN ('verify_email', 'reset_password')),
  token_hash bytea NOT NULL UNIQUE,           -- SHA-256 of the 256-bit token
  created_at timestamptz NOT NULL DEFAULT now(),
  expires_at timestamptz NOT NULL,            -- verify: 24 h, reset: 30 min
  used_at    timestamptz
);
CREATE INDEX email_tokens_account_idx ON email_tokens (account_id, purpose);

-- 0003_matches.sql
CREATE TABLE matches (
  id                      uuid PRIMARY KEY,   -- MatchId
  lobby_id                uuid NOT NULL,
  seed                    bigint NOT NULL,    -- u64 bit pattern
  rounds                  smallint NOT NULL CHECK (rounds BETWEEN 1 AND 100),
  round_time_limit_secs   integer NOT NULL CHECK (round_time_limit_secs BETWEEN 1 AND 3600),
  max_absent_round_starts smallint NOT NULL CHECK (max_absent_round_starts BETWEEN 1 AND 10),
  filter_mode             text NOT NULL CHECK (filter_mode IN ('allow', 'ban')),
  filter_game_ids         text[] NOT NULL,
  started_at              timestamptz NOT NULL,
  ended_at                timestamptz NOT NULL,
  ended_early             boolean NOT NULL
);
CREATE INDEX matches_ended_idx ON matches (ended_at DESC);

CREATE TABLE match_players (
  match_id     uuid NOT NULL REFERENCES matches(id) ON DELETE CASCADE,
  seat         smallint NOT NULL CHECK (seat BETWEEN 0 AND 3),
  player_id    uuid,                          -- NULL after account deletion (anonymized)
  final_rank   smallint NOT NULL,
  total_points integer NOT NULL,
  removed      boolean NOT NULL,
  PRIMARY KEY (match_id, seat)
);
CREATE INDEX match_players_player_idx ON match_players (player_id, match_id);

CREATE TABLE rounds (
  match_id    uuid NOT NULL REFERENCES matches(id) ON DELETE CASCADE,
  round_index smallint NOT NULL,
  game_id     text,                           -- NULL if skipped before selection
  status      text NOT NULL CHECK (status IN ('scored', 'skipped')),
  skip_reason text CHECK (skip_reason IN ('not_enough_players', 'empty_pool',
              'start_failed', 'no_results', 'invalid_report', 'game_aborted')),
  attempts    smallint NOT NULL,
  PRIMARY KEY (match_id, round_index)
);

CREATE TABLE round_results (
  match_id           uuid NOT NULL,
  round_index        smallint NOT NULL,
  seat               smallint NOT NULL,
  place              smallint NOT NULL,
  points             smallint NOT NULL,       -- scale 12, so 0..=12
  disqualification   text CHECK (disqualification IN ('timer_expired', 'left_round')),
  removed_this_round boolean NOT NULL DEFAULT false,
  PRIMARY KEY (match_id, round_index, seat),
  FOREIGN KEY (match_id, round_index) REFERENCES rounds ON DELETE CASCADE
);

-- 0004_replays.sql (written only when replay recording is enabled, STO-3)
CREATE TABLE replay_logs (
  match_id      uuid NOT NULL,
  round_index   smallint NOT NULL,
  game_id       text NOT NULL,
  game_seed     bigint NOT NULL,
  time_limit_ms bigint NOT NULL,
  format        smallint NOT NULL,            -- replay format version
  log           bytea NOT NULL,               -- MessagePack of ReplayLog
  PRIMARY KEY (match_id, round_index),
  FOREIGN KEY (match_id, round_index) REFERENCES rounds ON DELETE CASCADE
);
```

Notes:
- Participants of a round are the seats present in `round_results`; a skipped round has none.
- A match is written in one transaction after `MatchFinished`.
- Persisted by default: matches, players, rounds, results, accounts, sessions. Not persisted: live round state (STO-4).

### 2.4 Retention and deletion (SEC-14)

- Account deletion (`DELETE /api/v1/me`): delete credentials, email (replaced by a unique placeholder) and sessions, set `accounts.deleted_at`, set `match_players.player_id = NULL` for that account (match history stays, anonymized).
- Expired and revoked sessions, expired email tokens and pending accounts older than 24 h are purged by a daily job.
- Logs are retained at most 30 days; IP addresses are truncated or hashed.

### 2.5 Replay log format

A `ReplayLog` serialized with MessagePack: `{ game, game_seed, players, time_limit_ms, inputs: [(at_ms, GameInput)] }`, plus a `format` version column. Replaying: fresh instance, `initialize(game_seed, players, time_limit_ms)`, feed `(at_ms, input)` in order (REP-2).

### 2.6 Client state mirror (`client` crate)

`ClientState { phase, lobby: Option<LobbySnapshot>, round: Option<{ index, game_id, participants, clocks }>, totals, premove: Option<GameAction> }`. It is updated only from `ServerMessage`s and never trusts local time for authority; it counts down clocks between events for display only (LAT-8).

## 3. API contracts

### 3.1 Conventions

- Base path `/api/v1`. HTTP bodies are JSON (`Content-Type: application/json`). Game traffic is MessagePack over WebSocket.
- All auth responses send `Cache-Control: no-store`.
- HTTP errors use one shape; the WebSocket `Error` message carries the same codes (section 3.5):

```json
{ "error": { "code": "invalid_credentials", "message": "Invalid credentials" } }
```

- Session cookie: `__Host-smg_session=<token>; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=2592000`. Tokens never appear in URLs or logs (SEC-6).
- State-changing HTTP requests require a same-origin `Origin` header (CSRF defense in addition to `SameSite=Strict`).

### 3.2 HTTP endpoints

| Method and path | Auth | Purpose |
|---|---|---|
| `POST /api/v1/register` | none | Create a pending account and send a confirmation email (generic response) |
| `POST /api/v1/login` | none | Sign in |
| `POST /api/v1/email/verify` | none (token) | Confirm an email address and activate the account |
| `POST /api/v1/email/resend` | none | Re-send the confirmation (same response for any address) |
| `POST /api/v1/password/forgot` | none | Request a reset email (same response for any address) |
| `POST /api/v1/password/reset` | none (token) | Set a new password with a reset token |
| `POST /api/v1/password/change` | session | Change password (needs the current one; other sessions revoked) |
| `POST /api/v1/logout` | session | Revoke the current session |
| `POST /api/v1/logout-all` | session | Revoke all sessions of the account |
| `GET /api/v1/me` | session | Current account |
| `DELETE /api/v1/me` | session | Delete the account (requires password) |
| `POST /api/v1/ws-ticket` | session | Single-use ticket for native clients |
| `GET /api/v1/me/matches` | session | Own match history (cursor paging) |
| `GET /api/v1/matches/{match_id}` | session | One match with rounds (participants only) |
| `GET /ws` | session cookie or bearer ticket | WebSocket upgrade |
| `GET /healthz` | none | Liveness |
| `GET /readyz` | none | Readiness (database reachable, not over capacity) |
| `GET /metrics` | internal listener only | Prometheus text |

**`POST /api/v1/register`**

```json
// request
{ "username": "alice_99", "email": "alice@example.com", "password": "correct horse battery staple" }
// 202 response (no cookie; the same for every email)
{ "status": "check_your_email" }
```

| Status | `code` | When |
|---|---|---|
| 400 | `invalid_username` | not 3-20 characters of `A-Za-z0-9_-` |
| 400 | `weak_password` | under 10 characters, over 128, or in the breached list (SEC-4) |
| 409 | `username_taken` | username exists (case-insensitive) |
| 400 | `invalid_email` | not a valid email address |
| 429 | `rate_limited` | per-IP or per-account limit |
| 503 | `server_busy` | hash pool queue full (SEC-3) |

**`POST /api/v1/login`**

```json
// request
{ "username": "alice_99", "password": "correct horse battery staple" }
// 200 response (also sets the session cookie)
{ "player_id": "0b2f1c6e-5a8d-4b1e-9a43-7c1d5e2f9a10", "username": "alice_99" }
```

| Status | `code` | When |
|---|---|---|
| 401 | `invalid_credentials` | wrong username or password (same body and a dummy hash for unknown users, SEC-3, SEC-5) |
| 403 | `email_not_verified` | correct password, account not confirmed yet |
| 429 | `rate_limited` | throttled with exponential backoff; sends `Retry-After` |
| 503 | `server_busy` | hash pool queue full |

**Email verification and password reset**

```json
// POST /api/v1/email/verify        -> 204, or 400 invalid_token
{ "token": "..." }
// POST /api/v1/email/resend        -> always 202
{ "email": "alice@example.com" }
// POST /api/v1/password/forgot     -> always 202 { "status": "accepted" } (SEC-20)
{ "email": "alice@example.com" }
// POST /api/v1/password/reset      -> 204 (all sessions revoked), or 400 invalid_token / weak_password
{ "token": "...", "new_password": "a new long passphrase" }
// POST /api/v1/password/change     -> 204 (other sessions revoked), or 401 invalid_credentials / 400 weak_password
{ "current_password": "...", "new_password": "a new long passphrase" }
```

**`POST /api/v1/ws-ticket`**

```json
// 200 response
{ "ticket": "kP3x...base64url...", "expires_in_secs": 30 }
```

Native clients then call `GET /ws` with `Authorization: Bearer <ticket>`. Browsers use the cookie. A ticket works once.

**`GET /api/v1/me/matches?limit=20&cursor=...`**

```json
{
  "matches": [
    { "match_id": "...", "ended_at": "2026-11-04T10:12:00Z", "rounds": 5,
      "ended_early": false, "my_rank": 1, "my_total_points": 36,
      "opponent": { "player_id": "...", "username": "bob" } }
  ],
  "next_cursor": "..."
}
```

**`GET /api/v1/matches/{match_id}`** returns the match config, final ranking and each round's game, status, places and points. It never includes replay data.

### 3.3 WebSocket handshake

- `GET /ws` with header `Sec-WebSocket-Protocol: smg.v1`. The server checks `Origin` against `SMG_ALLOWED_ORIGINS` and the session (SEC-7).
- Binary frames only. Each frame is one `Envelope { v: u16, msg }` encoded with `rmp_serde::to_vec_named` (PRO-1, PRO-3). Maximum frame 64 KiB (PRO-10).
- If the player has a live lobby, the server sends `LobbyState`, `Resync`, `ScoreUpdate` and `PlayerStatus` after the upgrade (PRO-14); otherwise nothing until the client acts.
- A wrong `v` gets `Error { code: protocol_mismatch }` and the socket closes (PRO-2).

### 3.4 WebSocket messages

Names follow the spec (PRO-6, PRO-7). Types below show field shapes. Payload fields marked `bytes` use `serde_bytes`.

```rust
pub struct Envelope<T> { pub v: u16, pub msg: T }

// ---------- client -> server ----------
pub enum ClientMessage {
    CreateLobby { config: LobbyConfig, visibility: Visibility },
    JoinLobby { lobby_id: LobbyId, code: Option<JoinCode> },
    ListPublicLobbies,
    JoinQueue { preset: Preset },                // quick (MAT-1)
    LeaveQueue,
    LeaveLobby,
    SetReady { ready: bool },
    UpdateSettings { update: SettingsUpdate },   // no seed field (SRV-10, PRO-16)
    StartMatch,
    CancelLobby,
    GameInput { action: bytes },                 // game-specific encoding
    Ping { nonce: u64 },                         // optional, for the client's own latency display
}

pub struct LobbyConfig {                         // MatchConfig minus the seed
    pub rounds: u32,
    pub round_time_limit_secs: u64,
    pub max_absent_round_starts: u32,
    pub filter: GameFilterMode,
}

// ---------- server -> client ----------
pub enum ServerMessage {
    LobbyState { snapshot: LobbySnapshot },
    PublicLobbies { lobbies: Vec<PublicLobby> },
    QueueStatus { state: QueueState, waited_secs: u32 },      // MAT-6
    MatchFound { lobby_id: LobbyId, accept_secs: u32 },       // MAT-3
    RoundStarting { round_index: u32, game_id: GameId, game_name: String,
                    participants: Vec<PlayerId>, time_limit_secs: u64 },
    GameEvent { round_index: u32, payload: bytes },
    RoundScored { round_index: u32, placements: Vec<PlacementEntry> },
    RoundSkipped { round_index: u32, reason: SkipReason },
    ScoreUpdate { totals: Vec<(PlayerId, u32)> },
    PlayerStatus { player: PlayerId, status: PlayerStatus },   // present | away | removed
    Resync { phase: PhaseView, round_index: u32, is_participant: bool },   // PRO-14
    MatchFinished { ranking: Vec<RankEntry> },
    Error { code: ErrorCode, message: String },
    Pong { nonce: u64 },
}

pub struct PublicLobby {                         // PRO-15
    pub lobby_id: LobbyId, pub host_name: String, pub seats_taken: u8,
    pub max_seats: u8, pub rounds: u32, pub round_time_limit_secs: u64,
}
pub struct PlacementEntry { pub player: PlayerId, pub place: u8, pub points: u32, pub disqualified: Option<DisqualificationReason> }
pub enum Preset { Quick }                                  // MAT-1
pub enum QueueState { Waiting, Left }                      // MAT-6
pub enum PhaseView { Lobby, Starting, InRound, RoundFinished, MatchFinished, Cancelled }  // PRO-14
```

Behavior rules are in the spec: PRO-5, PRO-9, PRO-12, PRO-14, LAT-4, SEC-8.

### 3.5 Error codes

`ErrorCode` is shared by HTTP and WebSocket (PRO-8). Wire form is `snake_case`.

| Code | Source | HTTP status |
|---|---|---|
| `protocol_mismatch`, `malformed_message` | protocol | 400 |
| `unauthorized` | auth | 401 |
| `invalid_credentials`, `invalid_username`, `weak_password` | auth | 401 / 400 |
| `username_taken` | auth | 409 |
| `invalid_email`, `invalid_token` | auth | 400 |
| `email_not_verified` | auth | 403 |
| `rate_limited` | limits | 429 |
| `already_in_lobby` | lobby directory (SRV-11) | 409 |
| `server_busy` | admission control, hash pool | 503 |
| `wrong_state`, `not_host`, `lobby_full`, `invalid_join_code`, `settings_locked`, `not_all_ready`, `not_enough_players`, `unknown_player` | `LobbyError` | 409 / 403 |
| `invalid_config` (with `detail`: rounds, time_limit, absent_limit, empty_allow_list, duplicate_game, unknown_game, no_eligible_games) | `ConfigError` | 400 |
| `not_in_round`, `not_participant`, `player_left_round` | `InputError` | n/a (WebSocket only) |
| `input_rejected` | a game rejected the action (carried inside a `GameEvent`, not `Error`) | n/a |

### 3.6 Message sequences

**Lobby and match**
```text
A: CreateLobby ------------------------> server spawns actor
A <- LobbyState
B: JoinLobby --------------------------> A <- LobbyState, B <- LobbyState
A,B: SetReady -------------------------> LobbyState (settings locked while any Ready)
A: StartMatch -------------------------> server draws seed, engine enters Starting
both <- RoundStarting { round 0 }
both <- GameEvent* ... both <- RoundScored, ScoreUpdate
... repeat per round ...
both <- MatchFinished { ranking }
```

**Matchmaking**
```text
A: JoinQueue{quick} -> QueueStatus      B: JoinQueue{quick} -> pair A,B
server creates private lobby, seats both -> both <- MatchFound{lobby_id, 15}
A,B: SetReady{true} -> server sends StartMatch for the host -> RoundStarting
```

**Move**
```text
mover: GameInput { action } ----> server: lag_credit_ms from RTT -> actor tick
both <- GameEvent { applied action, both clocks, state byte }
opponent client: send queued premove immediately, if any
```

**Reconnect**
```text
socket drops -> Connection{false}; mid-round: PlayerStatus Away, 30 s grace (clocks keep running)
no return within the grace: PlayerLeft, game ends, opponent wins
client reconnects (cookie) -> LobbyState + Resync + ScoreUpdate + PlayerStatus
next round start: RoundStarting includes the player again
```

### 3.7 Game payloads and limits

Action and event encodings are in each game's section of the spec (22.3 to 22.7, GME-10). All limits and defaults are in the spec (SCP-4, PRO-10, SEC-6, SEC-9, SRV-3, SRV-4, LAT-7). Plan-only default: lobby creation is limited to 5 per minute per account.

## 4. Tech stack and why

| Area | Choice | Why |
|---|---|---|
| Language | Rust (stable, edition 2021), pinned in `rust-toolchain.toml`; nightly only for `cargo-fuzz` | No garbage-collector pauses (latency), small memory footprint (capacity), memory safety (`forbid(unsafe_code)`, VER-4) |
| Async runtime | `tokio` (multi-thread) | Cheap tasks for tens of thousands of matches; `time::pause` gives deterministic timer tests (TMR-7) |
| HTTP and WebSocket | `axum` | Built on tokio and hyper; first-class WebSocket upgrade; extractors keep handlers small |
| TLS | `rustls`, TLS 1.3 only | Memory-safe, no OpenSSL; in-process avoids an extra hop |
| Serialization | `serde`, `rmp-serde` (MessagePack), `serde_bytes` | Compact and fast; self-describing so optional fields can be added (PRO-3); libraries exist for TypeScript |
| Database | PostgreSQL 16+ with `sqlx` (compile-time checked queries, offline metadata in `.sqlx/`) | Relational data with constraints and indexes; parameterized queries only (SEC-11); room to scale out later |
| Password hashing | `argon2` (RustCrypto), Argon2id, PHC strings | OWASP-recommended; parameters upgradable (SEC-2) |
| Tokens and hashes | `sha2`, OS CSPRNG | Session tokens stored only as SHA-256 (SEC-6) |
| Email | `lettre` (SMTP) behind an `EmailSender` trait | Verification and reset mail (SEC-19, SEC-20); the provider can change without touching auth code |
| Logging and metrics | `tracing`, `tracing-subscriber` (JSON), `metrics` plus `metrics-exporter-prometheus`; Prometheus and Grafana | Structured logs with `match_id` and `lobby_id` (OBS-1); histograms for `tick_latency` (OBS-3) |
| Errors | `thiserror` | Typed errors, no panics in engine paths (FLR-8) |
| Deterministic RNG | `rand_chacha` (ChaCha8) and `rand_core`, hand-written rejection sampling | Reproducible selection across platforms (RNG-1 to RNG-5) |
| Testing | `cargo test`, `proptest`, tokio `test-util`, `criterion`, `cargo-fuzz` | Matches section 19 of the spec; property tests are free inside the test hook |
| Formal verification | Verus (`cargo verus`) on `superminigames-logic` only | Proves scoring, ranking, streak and phase logic (VER-10 to VER-18) |
| Supply chain | `cargo-deny`, `cargo-audit`, committed `Cargo.lock` | SEC-13, WSP-6 |
| Web UI | TypeScript, Vite, Svelte; `client` crate compiled to WASM with `wasm-bindgen` | Small bundle and fast rendering; the WASM crate keeps protocol, clock and premove logic identical to other clients. UI framework is a **plan decision**; React or Solid also work |
| Desktop and mobile | Tauri 2 around the same web UI | One UI for desktop and mobile; small footprint |
| Packaging | Docker multi-stage build; single static-ish binary plus migrations | Simple deploys; reproducible builds |
| Allocator | Default first; evaluate `mimalloc` or `jemalloc` in M4 under the load test | Fragmentation and RSS matter for PERF-2; decide with data |
| CI | GitHub Actions (or equivalent) | Runs what the editor hooks do not (section 6) |

Considered and rejected: JSON for game traffic (larger, slower, more allocation, PERF-4); SQLite (weak fit for multi-instance and concurrent writers later); a per-match OS thread (cannot reach the capacity target); a shared "rules" crate across games (rejected in WSP-9).

## 5. Repository layout

Crate package names keep the spec's prefix. The spec's `superminigames-games/<name>` path maps to `crates/games/<name>` with package name `superminigames-games-<name>`.

```text
superminigames/
+-- Cargo.toml                    # workspace: members = ["crates/*", "crates/games/*"]
+-- Cargo.lock                    # committed
+-- rust-toolchain.toml
+-- AGENTS.md                     # short rules for AI agents; points to spec/SPEC.md
+-- deny.toml                     # cargo-deny: bans, licenses, advisories, dependency rules
+-- .sqlx/                        # sqlx offline query metadata (committed)
+-- .github/workflows/            # ci.yml, nightly.yml (fuzz, load test, benches)
+-- spec/
|   +-- SPEC.md                   # requirements and rules (the spec wins)
|   +-- PLAN.md                   # this file
|   +-- TASK.md                   # master task list and subagent plan (orchestrator edits it)
|   +-- TRACEABILITY.md           # generated by traceability.py (VER-43)
|   +-- traceability.py           # generates and checks TRACEABILITY.md
|   +-- rules/                    # source-backed game rule briefs (T5.0)
+-- migrations/                   # 0001_accounts, 0002_email_tokens, 0003_matches, 0004_replays
+-- crates/
|   +-- logic/                    # package superminigames-logic (Verus-verified)
|   |   +-- Cargo.toml            # [package.metadata.verus] verify = true
|   |   +-- src/{lib,places,points,ranking,streak,phase,outcome}.rs
|   |   +-- tests/                # proptest properties mirroring the proofs
|   |   +-- benches/              # Criterion: places, points, ranking
|   +-- core/                     # package superminigames-core
|   |   +-- src/{lib,ids,config,game,outcome,scoring,rng,error}.rs
|   |   +-- src/test_support/     # game conformance suite (feature "test-support")
|   |   +-- tests/                # golden RNG files: golden_seed0.txt, golden_game_seed0.txt
|   |   +-- fuzz/                 # target: join_code_parse
|   +-- protocol/                 # package superminigames-protocol
|   |   +-- src/{lib,version,envelope,client_msg,server_msg,error_code}.rs
|   |   +-- tests/protocol/       # golden binary fixtures
|   |   +-- fuzz/                 # target: protocol_decode
|   +-- engine/                   # package superminigames-engine
|   |   +-- src/{lib,actor,lobby,registry,selection,round,presence,report,replay,error}.rs
|   |   +-- tests/                # lobby, round, presence, timer, failure, match tests (paused time)
|   |   +-- fuzz/                 # targets: engine_commands, replay_log_decode
|   |   +-- benches/              # round simulation, tick loop
|   +-- server/                   # package superminigames-server (binary)
|   |   +-- src/
|   |   |   +-- main.rs, config.rs, limits.rs, metrics.rs, supervisor.rs
|   |   |   +-- http/{mod,auth,password,history,health}.rs
|   |   |   +-- ws/{mod,upgrade,connection,codec,rtt}.rs
|   |   |   +-- lobby/{directory,host,fanout}.rs
|   |   |   +-- matchmaker/{queue,pairing,accept}.rs
|   |   |   +-- auth/{password,session,ticket,throttle,reset}.rs
|   |   |   +-- email/{sender,templates}.rs
|   |   |   +-- storage/{mod,pg,queue}.rs      # Storage trait + PostgreSQL implementation
|   |   +-- tests/                # integration: HTTP, WebSocket, security, load harness
|   |   +-- fuzz/                 # targets: auth_input, storage_load
|   +-- client/                   # package superminigames-client
|   |   +-- src/{lib,state,commands,premove,clock_display,wasm}.rs
|   |   +-- tests/
|   +-- games/
|       +-- chess/
|       +-- checkers/
|       +-- xiangqi/
|       +-- shogi/
|       +-- doushouqi/
|           (each: src/{lib,state,movegen,rules,clock,codec}.rs, tests/, fuzz/game_action_<name>, benches/)
+-- web/                          # TypeScript UI (Vite + Svelte), imports the WASM client package
|   +-- src/screens/              # Login, Register, ConfirmEmail, ResetPassword, ChangePassword, Home, LobbyList, LobbyRoom, Match, Results (UIX-1 to UIX-10)
|   +-- src/boards/<game>/        # one board component per game, uses that game crate as WASM (UIX-9)
+-- apps/
|   +-- tauri/                    # desktop and mobile wrapper around web/
+-- tools/
|   +-- loadtest/                 # scripted bot clients for 50,000 matches (T-PERF-1)
|   +-- botclient/                # CLI client for manual and integration testing
+-- ops/
|   +-- docker/Dockerfile
|   +-- compose.dev.yml           # server + PostgreSQL + Prometheus + Grafana
|   +-- grafana/dashboards/
|   +-- sysctl/99-smg.conf        # file descriptor and TCP tuning
+-- scripts/                      # check-deps.sh, check-verus-escapes.sh, check-clause-ids.sh
```

Naming rules:
- Test names include requirement IDs (for example `t_scr_2_draw_splits_points`).
- Every `requires` or `ensures` clause in `logic` carries a requirement-ID comment (VER-18).
- Game crates never import each other, `engine`, `server` or `protocol` (WSP-9).

## 6. Build, test and verification pipeline

### 6.1 What runs where

| Stage | Runs | Tools |
|---|---|---|
| Editor hook on every edit | `cargo check`, `cargo clippy --fix`, `cargo test`. The AI never runs these itself (VER-2) | hooks |
| On change to `logic` or verified rules | `cargo verus verify -p superminigames-logic` | Verus |
| On change to an untrusted-input path | `cargo fuzz run <target> -- -max_total_time=60` | cargo-fuzz |
| CI on every PR | build, tests, `cargo-deny`, `cargo-audit`, dependency-rule check, wasm build of `core` and `protocol`, Verus when `logic` changed, escape-hatch scan, clause-ID scan | CI |
| Nightly | each fuzz target for 10 minutes, load test T-PERF-1, Criterion benches (reported, non-blocking) | CI |
| Late stage | Criterion benchmarks (VER-30) | Criterion |

### 6.2 Test mapping

| Spec tests | Where they live |
|---|---|
| T-SCR-*, T-VER-* | `logic` (proptest and Verus), `core` adapters |
| T-RNG-*, T-POOL-* | `core` (RNG helpers), `engine` (pool and selection) |
| T-LOB-*, T-PRS-*, T-TMR-*, T-FLR-*, T-MTH-* | `engine/tests` with scripted fake games and paused time |
| T-GAM-*, T-REP-1 | `core/test_support` conformance suite, run by every game crate |
| T-GME-*, T-WSP-2 | each game crate |
| T-PRO-* | `protocol/tests` and `server/tests` |
| T-SEC-*, T-SRV-*, T-STO-*, T-MAT-*, T-OBS-1 | `server/tests` against a test PostgreSQL |
| T-UIX-* | `web/` tests |
| T-LAT-*, T-PERF-* | `server/tests` and `tools/loadtest` |
| T-WSP-1 | CI dependency check script |

### 6.3 Traceability

`spec/traceability.py` generates `spec/TRACEABILITY.md` from the spec. CI runs it with `--check` and fails if a MUST requirement has no test, CI check or review entry, or if the file is stale (VER-40 to VER-43).

### 6.4 Golden files

`golden_seed0.txt` and `golden_game_seed0.txt` are generated once by the first working implementation, reviewed by a human, and committed. After that any change fails CI (RNG-9).

## 7. Security implementation

Requirements and the threat model are in spec section 21. Implementation choices:
- Argon2id through the `argon2` crate; parameters come from `SMG_ARGON2_*` (defaults in SEC-2); hashing runs on a pool of `SMG_HASH_POOL_SIZE`.
- Session cookie `__Host-smg_session`; tokens stored as SHA-256 in `sessions`.
- All SQL through `sqlx` macros with offline metadata in `.sqlx/`.
- Pepper read from `SMG_PEPPER_FILE` when set.
- Verification and reset tokens: 256-bit OS-CSPRNG values stored as SHA-256 in `email_tokens` (SEC-19, SEC-20).
- The seed is drawn in the server at `StartMatch` (SRV-10).

## 8. Configuration and operations

### 8.1 Configuration (environment variables)

| Variable | Meaning | Default |
|---|---|---|
| `SMG_BIND_ADDR` | public listener | `0.0.0.0:443` |
| `SMG_METRICS_ADDR` | internal metrics and health listener | `127.0.0.1:9090` |
| `SMG_TLS_CERT`, `SMG_TLS_KEY` | PEM paths (reloaded on change) | required |
| `DATABASE_URL` | PostgreSQL connection | required |
| `SMG_ALLOWED_ORIGINS` | comma-separated origins for WebSocket and CSRF checks | required |
| `SMG_MAX_LIVE_MATCHES` | admission limit | 50000 |
| `SMG_HASH_POOL_SIZE` | concurrent Argon2id hashes | 8 |
| `SMG_ARGON2_M_KIB`, `SMG_ARGON2_T`, `SMG_ARGON2_P` | hash parameters | 19456, 2, 1 |
| `SMG_PEPPER_FILE` | path to the pepper secret | unset |
| `SMG_RECORD_REPLAYS` | enable replay logs | `false` |
| `SMG_DEBUG_SEED` | fixed match seed for tests and debug builds only (SRV-10) | unset |
| `SMG_EMAIL_DSN` | SMTP connection URL | required |
| `SMG_EMAIL_FROM` | sender address | required |
| `SMG_PUBLIC_BASE_URL` | base URL used in email links | required |
| `SMG_LOG` | log level filter | `info` |

### 8.2 Deployment

- One server instance behind DNS (v1), PostgreSQL managed or on a separate host. Prometheus scrapes `/metrics` on the internal listener; Grafana dashboards cover tick latency, live matches, disconnect rate, hash pool queue and selection distribution (OBS-3).
- OS tuning for 100,000 connections: raise file descriptor limits, set `tcp_rmem` and `tcp_wmem` ceilings, enable `SO_REUSEPORT` only if running multiple listeners. Keep the file in `ops/sysctl/`.
- Graceful shutdown (drain): stop accepting new lobbies, return `server_busy`, let running matches finish up to a deadline, then close. Matches still running at the deadline are lost (SRV-8).
- Backups: PostgreSQL point-in-time recovery; migrations run on deploy before the new binary starts.
- Email: send from a domain with SPF, DKIM and DMARC configured, through a transactional provider over SMTP.

### 8.3 Health

- `/healthz`: process is up.
- `/readyz`: database reachable, storage queue below its limit, live matches below `SMG_MAX_LIVE_MATCHES`.

## 9. Milestones

Each milestone ends with tests passing, the editor hooks clean, and the listed exit criteria met.

| # | Milestone | Scope | Exit criteria |
|---|---|---|---|
| M0 | Workspace and tooling | Workspace, toolchain, `deny.toml`, CI skeleton, hooks, `forbid(unsafe_code)`, dependency-rule script | CI green on an empty workspace; T-WSP-1 passes |
| M1 | `logic` and `core` | Scoring, ranking, streak, phase functions; types; `Game` trait; RNG helpers; conformance suite skeleton | T-SCR-*, T-RNG-* pass; golden files generated and reviewed; Verus proofs for the VER-14 properties pass |
| M2 | Engine without network | Lobby, match flow, selection, presence, timers, failure handling, replay recording, scripted fake games | T-LOB-*, T-PRS-*, T-TMR-*, T-FLR-*, T-MTH-*, T-REP-1 pass; `engine_commands` fuzz target runs clean for 10 minutes |
| M3 | Protocol and server skeleton | Protocol crate, axum, WebSocket gateway, accounts, sessions, tickets (migrations 0001, 0002), email verification and password reset, lobby hosting, matchmaking queue, bot client | T-PRO-*, T-SEC-*, T-SRV-*, T-MAT-* pass; two bot clients complete a match with a fake game |
| M4 | First game end to end: chess | Chess rules and clocks, lag credit, RTT, fan-out | T-GME-*, T-LAT-* pass; first load test: measure memory per match and `tick_latency`; decide TLS and allocator strategy (section 1.6) |
| M5 | Remaining games | Checkers, xiangqi, shogi, doushouqi | Each passes its reference rule suites, T-GME-*, `game_action_<name>` fuzz clean; spec section 22 rules reviewed by someone who knows each game |
| M6 | Persistence and observability | match and replay tables (migrations 0003, 0004; accounts and email tokens come in M3), storage writer, history API, metrics, dashboards | T-STO-* pass; kill-the-database test keeps matches running; `/readyz` behaves |
| M7 | Clients | WASM client crate, web UI (home, lobby list, lobby room, match screen) with premoves, then Tauri desktop and mobile | T-LAT-3 passes end to end; a full match is playable in browser and desktop |
| M8 | Hardening | Full fuzz set, T-PERF-1 at 50,000 matches, Criterion benchmarks, security review, penetration checklist | PERF-1 and PERF-2 met or targets formally revised; no open high-severity findings |

Critical path: M1 -> M2 -> M3 -> M4. M5 and M6 can overlap after M4. M7 can start after M3 using the bot-tested protocol.

## 10. Risks and open questions

| Risk | Effect | Mitigation |
|---|---|---|
| Per-connection TLS and socket memory exceeds the 100 KiB per match budget | Capacity target missed | Measure in M4; lower buffers; edge TLS termination as fallback |
| Verus supports only a subset of Rust | Some logic cannot be proven | Keep `logic` plain and primitive; cover the rest with proptest and fuzzing (VER-11, VER-15) |
| Game rules written from the spec's summaries contain errors | Wrong results | Reference suites (perft for chess), review by domain experts in M5 (GME-11) |
| Single instance is a single point of failure | Downtime loses live matches | Drain deploys; scaling path in section 1.8 |
| No account recovery or email | Locked-out users | Out of scope for v1 (DEC-31); decide before launch |
| Engine assistance (computer help) in chess-like games | Unfair play | Not solvable by the engine; out of scope; consider reporting tools later |
| Lag compensation can be abused by deliberately delaying pongs | Free clock time | Credit capped per move and per round, measured only by the server; monitor credit statistics |

Open decisions that block nothing yet: UI framework final choice, metrics dashboards, OAuth or guest login, account recovery, multi-region, doushouqi repetition rule (DEC-49).

## 11. Spec gaps found while planning

The nine gaps found during planning were fixed in the spec (DEC-50 to DEC-57).

## 12. Spec sections to plan sections

| Spec section | Plan section |
|---|---|
| 1 Scope and workspace | 1.2, 5 |
| 3 Core types | 2.1 |
| 4 to 10 Match rules | 1.4, 1.7, 6.2 (tests) |
| 11 Engine architecture | 1.3 |
| 13 Game runtime and replay | 2.5, 3.7 |
| 14 Protocol | 3.3, 3.4, 3.5 |
| 15 Server, storage, observability | 1.3, 2.2, 2.3, 8 |
| 19 Verification policy | 6 |
| 20 Latency and performance | 1.5, 1.6 |
| 21 Security | 7 |
| 22 Initial games | 3.7, M4, M5 |
| 23 Joining and UI | 1.1, 2.2, 3.4, 3.6, 5, M3, M7 |
