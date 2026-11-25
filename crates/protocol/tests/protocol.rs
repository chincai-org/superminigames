//! Protocol checks: T-PRO-1, T-PRO-2, T-PRO-5, T-PRO-7 (and PRO-3, PRO-4, PRO-9,
//! PRO-15, which those checks imply).
//!
//! The golden fixtures under `tests/protocol/` are committed bytes, not a
//! re-encoding of the function under test: `t_pro_1_golden_fixtures_match`
//! compares against them and checks that the expected field names appear
//! literally in the bytes, so an accidental rename or a move to positional
//! encoding fails the test.
//!
//! Regenerate them (only when the wire format really changes, and then inspect
//! the diff by hand) with:
//! `cargo test --test protocol -- --ignored regenerate_golden_fixtures`

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::Serialize;
use superminigames_core::config::GameFilterMode;
use superminigames_core::game::{GameAction, Payload};
use superminigames_core::ids::{GameId, LobbyId, Player, PlayerId};
use superminigames_protocol::client_msg::SettingsUpdate;
use superminigames_protocol::{
    ClientEnvelope, ClientMessage, ErrorCode, LobbyConfig, LobbySnapshot, LobbyVisibility,
    MAX_MESSAGE_BYTES, PROTOCOL_VERSION, Phase, Placement, PlayerPoints, PlayerStatus,
    ProtocolError, PublicLobby, QueueState, RECONNECT_GRACE_SECS, RankEntry, Seat, ServerEnvelope,
    ServerMessage, SkipReason, Standing, decode_client, decode_server, encode_client,
    encode_server,
};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn player(byte: u8) -> PlayerId {
    PlayerId(uuid::Uuid::from_bytes([byte; 16]))
}

fn lobby(byte: u8) -> LobbyId {
    LobbyId(uuid::Uuid::from_bytes([byte; 16]))
}

fn config() -> LobbyConfig {
    LobbyConfig {
        rounds: 3,
        round_time_limit_secs: 360,
        max_absent_round_starts: 3,
        filter: GameFilterMode::AllowList(vec![GameId("chess".to_string())]),
    }
}

/// One value per `ClientMessage` variant (PRO-6). A new variant must be added
/// here, or `t_pro_1_every_client_message_round_trips` stops covering it.
fn client_messages() -> Vec<ClientMessage> {
    vec![
        ClientMessage::CreateLobby {
            config: config(),
            visibility: LobbyVisibility::Private,
        },
        ClientMessage::JoinLobby {
            lobby_id: lobby(1),
            code: Some("AB12CD".to_string()),
        },
        ClientMessage::ListPublicLobbies,
        ClientMessage::JoinQueue {
            preset: "quick".to_string(),
        },
        ClientMessage::LeaveQueue,
        ClientMessage::LeaveLobby,
        ClientMessage::SetReady { ready: true },
        ClientMessage::UpdateSettings {
            update: SettingsUpdate {
                rounds: Some(5),
                ..SettingsUpdate::default()
            },
        },
        ClientMessage::StartMatch,
        ClientMessage::CancelLobby,
        ClientMessage::GameInput {
            action: GameAction(vec![0, 12, 28, 0]),
        },
        ClientMessage::Ping { nonce: 42 },
    ]
}

/// One value per `ServerMessage` variant (PRO-7).
fn server_messages() -> Vec<ServerMessage> {
    vec![
        ServerMessage::LobbyState {
            snapshot: LobbySnapshot {
                lobby_id: lobby(1),
                host: player(2),
                seats: vec![
                    Seat {
                        player: Player {
                            id: player(2),
                            name: "alice".to_string(),
                        },
                        ready: true,
                    },
                    Seat {
                        player: Player {
                            id: player(3),
                            name: "bob".to_string(),
                        },
                        ready: false,
                    },
                ],
                config: config(),
                private: true,
                settings_locked: true,
            },
        },
        ServerMessage::PublicLobbies {
            lobbies: vec![PublicLobby {
                lobby_id: lobby(1),
                host_name: "alice".to_string(),
                seats_taken: 1,
                max_seats: 2,
                rounds: 3,
                round_time_limit_secs: 360,
            }],
        },
        ServerMessage::QueueStatus {
            state: QueueState::Queued,
            waited_secs: 12,
        },
        ServerMessage::MatchFound {
            lobby_id: lobby(1),
            accept_secs: 15,
        },
        ServerMessage::RoundStarting {
            round_index: 0,
            game_id: GameId("chess".to_string()),
            game_name: "Chess".to_string(),
            participants: vec![player(2), player(3)],
            time_limit_secs: 360,
        },
        ServerMessage::GameEvent {
            payload: Payload(vec![0, 12, 28, 0, 0x10, 0x27, 0xc8, 0x20, 0]),
        },
        ServerMessage::RoundScored {
            round_index: 0,
            placements: vec![
                Placement {
                    player: player(2),
                    place: 1,
                },
                Placement {
                    player: player(3),
                    place: 2,
                },
            ],
            points: vec![
                PlayerPoints {
                    player: player(2),
                    points: 12,
                },
                PlayerPoints {
                    player: player(3),
                    points: 0,
                },
            ],
        },
        ServerMessage::RoundSkipped {
            round_index: 1,
            reason: SkipReason::EmptyPool,
        },
        ServerMessage::ScoreUpdate {
            totals: vec![
                PlayerPoints {
                    player: player(2),
                    points: 12,
                },
                PlayerPoints {
                    player: player(3),
                    points: 0,
                },
            ],
        },
        ServerMessage::PlayerStatus {
            player: player(3),
            status: PlayerStatus::Away,
        },
        ServerMessage::Resync {
            phase: Phase::InRound,
            round_index: 0,
            is_participant: false,
        },
        ServerMessage::MatchFinished {
            ranking: vec![
                RankEntry {
                    player: player(2),
                    rank: 1,
                    total_points: 30,
                    standing: Standing::InMatch,
                },
                RankEntry {
                    player: player(3),
                    rank: 2,
                    total_points: 18,
                    standing: Standing::Removed,
                },
            ],
        },
        ServerMessage::Error {
            code: ErrorCode::WrongState,
            message: "the lobby is starting".to_string(),
        },
        ServerMessage::Pong { nonce: 42 },
    ]
}

// ---------------------------------------------------------------------------
// T-PRO-1: round-trip every variant (PRO-1, PRO-3)
// ---------------------------------------------------------------------------

#[test]
// REQ: PRO-1, PRO-3, T-PRO-1
fn t_pro_1_every_client_message_round_trips() {
    for message in client_messages() {
        let envelope = ClientEnvelope::new(message.clone());
        let frame = encode_client(&envelope).expect("encoding a client frame works");
        let decoded = decode_client(&frame).expect("a frame this build wrote decodes");
        assert_eq!(decoded, envelope, "round trip of {message:?}");
        assert!(decoded.is_supported_version(), "PRO-2: version is stamped");
        // PRO-3: named maps, so the variant name is a literal key in the frame.
        let variant = named_variants(&message);
        for name in variant {
            assert!(
                contains(&frame, name.as_bytes()),
                "PRO-3: {name} should appear as a named field in {message:?}"
            );
        }
    }
}

#[test]
// REQ: PRO-1, PRO-3, T-PRO-1
fn t_pro_1_every_server_message_round_trips() {
    for message in server_messages() {
        let envelope = ServerEnvelope::new(message.clone());
        let frame = encode_server(&envelope).expect("encoding a server frame works");
        let decoded = decode_server(&frame).expect("a frame this build wrote decodes");
        assert_eq!(decoded, envelope, "round trip of {message:?}");
    }
}

#[test]
// REQ: PRO-1, PRO-2
fn t_pro_2_the_envelope_stamps_the_current_version() {
    let envelope = ClientEnvelope::new(ClientMessage::ListPublicLobbies);
    assert_eq!(envelope.v, PROTOCOL_VERSION);
    assert_eq!(PROTOCOL_VERSION, 1);
    assert_eq!(RECONNECT_GRACE_SECS, 30, "UIX-7 reads this constant");
}

#[test]
// REQ: PRO-2, PRO-8, T-PRO-2
fn t_pro_2_a_wrong_version_is_protocol_mismatch() {
    // Same shape as a real frame, with a different version number (PRO-2).
    #[derive(Serialize)]
    struct WrongVersion {
        v: u16,
        msg: ClientMessage,
    }

    for version in [0u16, 2, u16::MAX] {
        let frame = rmp_serde::to_vec_named(&WrongVersion {
            v: version,
            msg: ClientMessage::ListPublicLobbies,
        })
        .expect("encoding the probe works");
        let error = decode_client(&frame).expect_err("PRO-2 rejects another version");
        assert_eq!(error, ProtocolError::UnsupportedVersion { got: version });
        assert_eq!(
            error.code(),
            ErrorCode::ProtocolMismatch,
            "PRO-2: the server answers protocol_mismatch"
        );
    }
}

#[test]
// REQ: PRO-10, T-PRO-5
fn t_pro_10_a_frame_over_64_kib_is_malformed() {
    assert_eq!(MAX_MESSAGE_BYTES, 64 * 1024, "PRO-10: the limit is 64 KiB");
    let oversized = vec![0u8; MAX_MESSAGE_BYTES + 1];
    let error = decode_client(&oversized).expect_err("PRO-10: oversize is rejected");
    assert_eq!(
        error,
        ProtocolError::TooLarge {
            len: MAX_MESSAGE_BYTES + 1
        }
    );
    assert_eq!(error.code(), ErrorCode::MalformedMessage);
    assert!(
        decode_client(&vec![0u8; MAX_MESSAGE_BYTES]).is_err(),
        "PRO-10: a 64 KiB frame of zeros is inside the limit but not a message"
    );
}

#[test]
// REQ: PRO-1, LAT-5, T-LAT-2 (documents the spec conflict)
fn t_lat_2_chess_move_fits_the_40_byte_budget() {
    // CHS-3 defines a four-byte ordinary move payload. With the mandatory
    // named MessagePack envelope, this is 33 bytes, so LAT-5's 40-byte limit
    // now holds the ordinary chess move with a little room for future fields.
    let input = encode_client(&ClientEnvelope::new(ClientMessage::GameInput {
        action: GameAction(vec![0, 12, 28, 0]),
    }))
    .expect("encodes");
    assert_eq!(
        input.len(),
        33,
        "PRO-1/PRO-3/PRO-6 wire shape and CHS-3 move"
    );
    assert!(input.len() <= 40, "T-LAT-2 budget");
}

#[test]
// REQ: PRO-1, PRO-10
fn t_pro_10_garbage_is_malformed_not_a_panic() {
    for frame in [
        vec![],
        vec![0x00],
        vec![0x81, 0xa1, b'v', 0x01], // map with only `v`, no `msg`
        vec![0xc1],                   // reserved msgpack byte
        vec![0x92, 0x01, 0x02],       // an array, not a map
        vec![0x81, 0xa1, b'v', 0xa1, b'x'], // `v` is not a number
    ] {
        let error = decode_client(&frame).expect_err("PRO-1: not a message");
        assert_eq!(
            error.code(),
            ErrorCode::MalformedMessage,
            "frame {frame:02x?}"
        );
    }
}

// ---------------------------------------------------------------------------
// T-PRO-7: no seed, no join code, no player id where they do not belong
// (PRO-15, PRO-16)
// ---------------------------------------------------------------------------

#[test]
// REQ: PRO-16, SRV-10, T-PRO-7
fn t_pro_16_a_seed_field_is_rejected() {
    // A `CreateLobby` whose config carries a seed (PRO-16, SRV-10).
    let nested = msgpack(serde_json::json!({
        "v": PROTOCOL_VERSION,
        "msg": {
            "CreateLobby": {
                "config": {
                    "rounds": 3,
                    "round_time_limit_secs": 360,
                    "max_absent_round_starts": 3,
                    "filter": "BanList",
                    "seed": 7
                },
                "visibility": "public"
            }
        }
    }));
    let error = decode_client(&nested).expect_err("PRO-16: a seed is rejected");
    assert_eq!(error.code(), ErrorCode::MalformedMessage);

    // The same field one level deeper, inside `UpdateSettings.update` (T-PRO-7).
    let update = msgpack(serde_json::json!({
        "v": PROTOCOL_VERSION,
        "msg": {
            "UpdateSettings": {
                "update": { "seed": 7 }
            }
        }
    }));
    let error = decode_client(&update).expect_err("PRO-16: a seed is rejected here too");
    assert_eq!(error.code(), ErrorCode::MalformedMessage);
    // Control: an empty update decodes, so the rejection is the `seed` field and
    // not a missing optional field (T-PRO-7).
    let empty_update = msgpack(serde_json::json!({
        "v": PROTOCOL_VERSION,
        "msg": { "UpdateSettings": { "update": {} } }
    }));
    let decoded = decode_client(&empty_update).expect("an empty settings update is valid");
    let ClientMessage::UpdateSettings { update } = decoded.msg else {
        panic!("PRO-16: the control frame is an UpdateSettings");
    };
    assert_eq!(update.rounds, None);

    // And an unknown field on the message itself.
    let unknown = msgpack(serde_json::json!({
        "v": PROTOCOL_VERSION,
        "msg": { "CreateLobby": { "config": { "rounds": 3, "round_time_limit_secs": 360, "max_absent_round_starts": 3, "filter": { "BanList": [] } }, "visibility": "public", "score": 1 } }
    }));
    assert_eq!(
        decode_client(&unknown).expect_err("PRO-16").code(),
        ErrorCode::MalformedMessage
    );

    // Control: the same frame without `score` decodes, so the rejection above
    // is caused by the unknown field and not by an unrelated invalid value.
    let control = msgpack(serde_json::json!({
        "v": PROTOCOL_VERSION,
        "msg": { "CreateLobby": { "config": { "rounds": 3, "round_time_limit_secs": 360, "max_absent_round_starts": 3, "filter": { "BanList": [] } }, "visibility": "public" } }
    }));
    let decoded = decode_client(&control).expect("the control frame is valid");
    let ClientMessage::CreateLobby { config, visibility } = decoded.msg else {
        panic!("PRO-16: the control frame is a CreateLobby");
    };
    assert_eq!(visibility, LobbyVisibility::Public);
    assert_eq!(config.rounds, 3);
}

#[test]
// REQ: PRO-15, SRV-5, T-PRO-7
fn t_pro_15_a_public_lobby_entry_carries_no_secret() {
    let lobby_id = lobby(7);
    let message = ServerMessage::PublicLobbies {
        lobbies: vec![PublicLobby {
            lobby_id,
            host_name: "alice".to_string(),
            seats_taken: 1,
            max_seats: 2,
            rounds: 3,
            round_time_limit_secs: 360,
        }],
    };
    let frame = encode_server(&ServerEnvelope::new(message.clone())).expect("encodes");
    let keys = keys_of(&frame);
    for forbidden in [
        "seed",
        "code",
        "join_code",
        "player",
        "player_id",
        "players",
    ] {
        assert!(
            !keys.contains(forbidden),
            "PRO-15: a public lobby entry must not carry `{forbidden}`, saw {keys:?}"
        );
    }
    assert!(
        keys.contains("host_name"),
        "PRO-15 names the host, not an id"
    );
    assert!(keys.contains("lobby_id"), "PRO-15 keeps the lobby id");

    // The host's name is present, the host's player id is not.
    let decoded = decode_server(&frame).expect("decodes");
    assert_eq!(decoded, ServerEnvelope::new(message));
    let host_id = player(2);
    assert!(
        !contains(&frame, &host_id.0.as_bytes()[..]),
        "PRO-15: a player id must not appear in a public lobby entry"
    );
}

#[test]
// REQ: PRO-4, PRO-16, SRV-10
fn t_pro_4_a_lobby_snapshot_has_no_seed_or_replay() {
    let snapshot = LobbySnapshot {
        lobby_id: lobby(1),
        host: player(2),
        seats: vec![Seat {
            player: Player {
                id: player(2),
                name: "alice".to_string(),
            },
            ready: true,
        }],
        config: config(),
        private: false,
        settings_locked: true,
    };
    let frame = encode_server(&ServerEnvelope::new(ServerMessage::LobbyState { snapshot }))
        .expect("encodes");
    let keys = keys_of(&frame);
    for forbidden in ["seed", "replay", "rng", "inputs"] {
        assert!(
            !keys.contains(forbidden),
            "PRO-4: engine state `{forbidden}` must not reach a client"
        );
    }
}

#[test]
// REQ: PRO-5, SEC-8
fn t_pro_5_no_client_message_carries_a_score_or_time() {
    for message in client_messages() {
        let frame = encode_client(&ClientEnvelope::new(message.clone())).expect("encodes");
        let keys = keys_of(&frame);
        for forbidden in [
            "score",
            "scores",
            "place",
            "places",
            "outcome",
            "points",
            "total_points",
            "timestamp",
            "at_ms",
            "received_at",
            "lag_credit_ms",
        ] {
            assert!(
                !keys.contains(forbidden),
                "PRO-5: a client must not send `{forbidden}` ({message:?})"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// PRO-1: opaque payloads are bytes, not integer arrays
// ---------------------------------------------------------------------------

#[test]
// REQ: PRO-1
fn t_pro_1_a_game_payload_travels_as_bytes() {
    let payload: Vec<u8> = (0..200u8).collect();
    let frame = encode_server(&ServerEnvelope::new(ServerMessage::GameEvent {
        payload: Payload(payload.clone()),
    }))
    .expect("encodes");
    // MessagePack `bin8` (0xc4) with the length 200, then the raw bytes.
    let expected: Vec<u8> = [vec![0xc4, 200], payload].concat();
    assert!(
        contains(&frame, &expected),
        "PRO-1: the payload must be one bin field, not an array of integers"
    );
    assert!(
        !contains(&frame, &[0xdc, 0x00, 200]),
        "PRO-1: an array16 of 200 integers would be the wrong encoding"
    );
}

// ---------------------------------------------------------------------------
// Error codes
// ---------------------------------------------------------------------------

#[test]
// REQ: PRO-8
fn t_pro_8_every_error_code_is_snake_case_on_the_wire() {
    let codes = [
        ErrorCode::ProtocolMismatch,
        ErrorCode::MalformedMessage,
        ErrorCode::Unauthorized,
        ErrorCode::RateLimited,
        ErrorCode::ServerBusy,
        ErrorCode::AlreadyInLobby,
        ErrorCode::InvalidCredentials,
        ErrorCode::InvalidUsername,
        ErrorCode::WeakPassword,
        ErrorCode::UsernameTaken,
        ErrorCode::InvalidEmail,
        ErrorCode::EmailNotVerified,
        ErrorCode::InvalidToken,
        ErrorCode::WrongState,
        ErrorCode::NotHost,
        ErrorCode::LobbyFull,
        ErrorCode::InvalidJoinCode,
        ErrorCode::SettingsLocked,
        ErrorCode::NotAllReady,
        ErrorCode::NotEnoughPlayers,
        ErrorCode::UnknownPlayer,
        ErrorCode::InvalidConfig,
        ErrorCode::NotInRound,
        ErrorCode::NotParticipant,
        ErrorCode::PlayerLeftRound,
    ];
    for code in codes {
        let json = serde_json::to_value(code).expect("codes serialize");
        assert_eq!(json, serde_json::json!(code.as_str()), "PRO-8: {code:?}");
        assert!(
            code.as_str()
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_'),
            "PRO-8: {code:?} must be snake_case"
        );
    }
}

// ---------------------------------------------------------------------------
// Golden fixtures (T-PRO-1)
// ---------------------------------------------------------------------------

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/protocol")
}

/// The four fixtures and what each must contain literally (PRO-3).
fn golden() -> Vec<(&'static str, Vec<u8>, Vec<&'static str>)> {
    let messages = client_messages();
    let server = server_messages();
    vec![
        (
            "client_create_lobby.msgpack",
            encode_client(&ClientEnvelope::new(messages[0].clone())).expect("encodes"),
            vec!["v", "msg", "CreateLobby", "config", "rounds", "visibility"],
        ),
        (
            "client_game_input.msgpack",
            encode_client(&ClientEnvelope::new(messages[10].clone())).expect("encodes"),
            vec!["GameInput", "action"],
        ),
        (
            "server_round_starting.msgpack",
            encode_server(&ServerEnvelope::new(server[4].clone())).expect("encodes"),
            vec!["RoundStarting", "round_index", "game_id", "participants"],
        ),
        (
            "server_match_finished.msgpack",
            encode_server(&ServerEnvelope::new(server[11].clone())).expect("encodes"),
            vec!["MatchFinished", "ranking", "rank", "standing"],
        ),
    ]
}

#[test]
// REQ: PRO-1, PRO-3, T-PRO-1
fn t_pro_1_golden_fixtures_match_byte_for_byte() {
    for (name, expected, fields) in golden() {
        let path = fixtures_dir().join(name);
        let committed = std::fs::read(&path).unwrap_or_else(|error| {
            panic!(
                "T-PRO-1: {} must be committed ({error}); regenerate with \
                 `cargo test --test protocol -- --ignored regenerate_golden_fixtures`",
                path.display()
            )
        });
        assert_eq!(
            committed,
            expected,
            "T-PRO-1: {} no longer matches the wire format",
            path.display()
        );
        for field in fields {
            assert!(
                contains(&committed, field.as_bytes()),
                "T-PRO-1: {} should name `{field}`, so the fixture is a named map (PRO-3)",
                path.display()
            );
        }
    }
}

#[test]
#[ignore = "rewrites the committed golden fixtures; run by hand after a deliberate format change"]
// REQ: PRO-1, T-PRO-1
fn regenerate_golden_fixtures() {
    let dir = fixtures_dir();
    std::fs::create_dir_all(&dir).expect("the fixture directory can be created");
    for (name, bytes, _) in golden() {
        std::fs::write(dir.join(name), &bytes).expect("the fixture can be written");
    }
}

// ---------------------------------------------------------------------------
// JSON debug feature (PRO-1)
// ---------------------------------------------------------------------------

#[cfg(feature = "json")]
#[test]
// REQ: PRO-1
fn t_pro_1_the_json_debug_feature_round_trips() {
    use superminigames_protocol::{decode_client_json, encode_client_json};
    for message in client_messages() {
        let envelope = ClientEnvelope::new(message);
        let text = encode_client_json(&envelope).expect("encodes as JSON");
        assert_eq!(
            decode_client_json(&text).expect("decodes from JSON"),
            envelope
        );
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn msgpack(value: serde_json::Value) -> Vec<u8> {
    rmp_serde::to_vec_named(&value).expect("a JSON value encodes as MessagePack")
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty()
        || haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

/// The variant name of a message, as it appears in the externally tagged wire
/// form (PRO-3). The wire's named map is verified by looking for the literal
/// MessagePack string; generic JSON decoding cannot represent `serde_bytes` bin
/// values (UUIDs and game payloads) and would change the contract under test.
fn named_variants(message: &ClientMessage) -> Vec<String> {
    let frame = encode_client(&ClientEnvelope::new(message.clone())).expect("encodes");
    let names = [
        "CreateLobby",
        "JoinLobby",
        "ListPublicLobbies",
        "JoinQueue",
        "LeaveQueue",
        "LeaveLobby",
        "SetReady",
        "UpdateSettings",
        "StartMatch",
        "CancelLobby",
        "GameInput",
        "Ping",
    ];
    names
        .into_iter()
        .filter(|name| contains(&frame, name.as_bytes()))
        .map(str::to_owned)
        .collect()
}

/// MessagePack map keys are encoded as literal string bytes. Scan the keys that
/// are security relevant here, rather than decoding opaque binary fields into a
/// JSON value (which is intentionally not possible for UUID/bin payloads).
fn keys_of(frame: &[u8]) -> BTreeSet<String> {
    let names = [
        "v",
        "msg",
        "config",
        "rounds",
        "round_time_limit_secs",
        "max_absent_round_starts",
        "filter",
        "visibility",
        "seed",
        "score",
        "scores",
        "place",
        "places",
        "outcome",
        "points",
        "total_points",
        "timestamp",
        "at_ms",
        "received_at",
        "lag_credit_ms",
        "lobby_id",
        "host_name",
        "seats_taken",
        "max_seats",
        "player",
        "player_id",
        "players",
        "code",
        "join_code",
        "replay",
        "rng",
        "inputs",
        "GameEvent",
        "payload",
    ];
    names
        .into_iter()
        .filter(|name| contains(frame, name.as_bytes()))
        .map(str::to_owned)
        .collect()
}
