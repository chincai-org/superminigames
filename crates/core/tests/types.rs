//! T1.4: the TYP-1 serde round-trips and the small invariants of the core
//! types (`JoinCode` validation, `GameId` byte ordering).

use superminigames_core::config::{
    GameFilterMode, MAX_PLAYERS, MIN_PLAYERS, MatchConfig, SettingsUpdate,
};
use superminigames_core::error::ConfigError;
use superminigames_core::game::{
    GameAction, GameEvent, GameInput, GameResult, Payload, ScoringMode, TimeModel,
};
use superminigames_core::ids::{GameId, JoinCode, LobbyId, MatchId, Player, PlayerId, Visibility};
use superminigames_core::outcome::{DisqualificationReason, PlayerOutcome};
use uuid::Uuid;

fn player_id(n: u128) -> PlayerId {
    // REQ: TYP-1 — the engine never generates ids (TYP-3), so tests build them.
    PlayerId(Uuid::from_u128(n))
}

fn round_trip<T>(value: &T) -> T
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let json = serde_json::to_string(value).expect("TYP-1: type must serialize");
    serde_json::from_str(&json).expect("TYP-1: type must deserialize")
}

#[test]
fn t_typ_1_ids_and_player_round_trip() {
    // REQ: TYP-1
    let player = Player {
        id: player_id(1),
        name: "ana".to_string(),
    };
    assert_eq!(round_trip(&player), player);
    assert_eq!(round_trip(&player_id(2)), player_id(2));
    assert_eq!(round_trip(&LobbyId(Uuid::nil())), LobbyId(Uuid::nil()));
    assert_eq!(round_trip(&MatchId(Uuid::nil())), MatchId(Uuid::nil()));
    assert_eq!(
        round_trip(&GameId("chess".to_string())),
        GameId("chess".to_string())
    );
}

#[test]
fn t_typ_1_config_round_trips() {
    // REQ: TYP-1
    assert_eq!(MIN_PLAYERS, 2);
    assert_eq!(MAX_PLAYERS, 2);

    let filter = GameFilterMode::AllowList(vec![GameId("chess".to_string())]);
    assert_eq!(round_trip(&filter), filter);
    let ban = GameFilterMode::BanList(vec![GameId("shogi".to_string())]);
    assert_eq!(round_trip(&ban), ban);

    let config = MatchConfig {
        rounds: 3,
        round_time_limit_secs: 360,
        seed: 0,
        max_absent_round_starts: 3,
        filter: filter.clone(),
    };
    assert_eq!(round_trip(&config), config);

    let update = SettingsUpdate {
        rounds: Some(5),
        round_time_limit_secs: None,
        max_absent_round_starts: Some(2),
        filter: Some(filter),
        visibility: Some(Visibility::Private(
            JoinCode::new("ABC123").expect("valid code"),
        )),
    };
    assert_eq!(round_trip(&update), update);
    assert_eq!(
        round_trip(&SettingsUpdate::default()),
        SettingsUpdate::default()
    );
}

#[test]
fn t_typ_1_visibility_round_trips() {
    // REQ: TYP-1
    assert_eq!(round_trip(&Visibility::Public), Visibility::Public);
    let private = Visibility::Private(JoinCode::new("Z9Y8X7").expect("valid code"));
    assert_eq!(round_trip(&private), private);
}

#[test]
fn t_typ_1_game_contract_types_round_trip() {
    // REQ: TYP-1
    let id = player_id(7);
    assert_eq!(round_trip(&ScoringMode::Placement), ScoringMode::Placement);
    assert_eq!(
        round_trip(&TimeModel::PlayerClocks),
        TimeModel::PlayerClocks
    );

    let action = GameAction(vec![1, 2, 3]);
    assert_eq!(round_trip(&action), action);
    let payload = Payload(vec![0, 255]);
    assert_eq!(round_trip(&payload), payload);

    let inputs = [
        GameInput::Player {
            player: id,
            action: action.clone(),
            lag_credit_ms: 1000,
        },
        GameInput::PlayerLeft(id),
        GameInput::TimeAdvanced,
    ];
    for input in inputs {
        assert_eq!(round_trip(&input), input);
    }

    let events = [
        GameEvent::Broadcast(payload.clone()),
        GameEvent::ToPlayer {
            player: id,
            payload: payload.clone(),
        },
        GameEvent::InputRejected {
            player: id,
            reason: "not your turn".to_string(),
        },
    ];
    for event in events {
        assert_eq!(round_trip(&event), event);
    }

    let outcomes = [
        (id, PlayerOutcome::Placed(1)),
        (id, PlayerOutcome::Scored(-5)),
        (
            id,
            PlayerOutcome::NoResult(DisqualificationReason::TimerExpired),
        ),
        (
            id,
            PlayerOutcome::NoResult(DisqualificationReason::LeftRound),
        ),
    ];
    for (id, outcome) in outcomes {
        assert_eq!(round_trip(&outcome), outcome);
        assert_eq!(
            superminigames_core::outcome::outcome_of(&[(id, outcome)], id),
            Some(outcome)
        );
    }

    let result = GameResult {
        outcomes: vec![(id, PlayerOutcome::Placed(2))],
    };
    assert_eq!(round_trip(&result), result);
}

#[test]
fn t_typ_1_action_payloads_are_byte_only() {
    // REQ: TYP-1 — JSON shows a byte array, not a string or an array of numbers
    // re-encoded through UTF-8.
    let action = GameAction(vec![0x41, 0x80, 0xff]);
    let json = serde_json::to_string(&action).expect("TYP-1: GameAction must serialize");
    assert_eq!(json, "[65,128,255]");
}

#[test]
fn t_typ_1_game_id_ordering_is_byte_order() {
    // REQ: TYP-1
    let mut ids = vec![
        GameId("b".to_string()),
        GameId("a".to_string()),
        GameId("ab".to_string()),
        GameId("A".to_string()),
    ];
    ids.sort();
    assert_eq!(
        ids,
        vec![
            GameId("A".to_string()),
            GameId("a".to_string()),
            GameId("ab".to_string()),
            GameId("b".to_string()),
        ]
    );
}

#[test]
fn t_typ_1_join_code_accepts_exactly_six_uppercase_alphanumerics() {
    // REQ: TYP-1
    let code = JoinCode::new("ABC123").expect("valid code");
    assert_eq!(code.as_str(), "ABC123");
    assert_eq!(round_trip(&code), code);

    for bad in [
        "", "ABC12", "ABC1234", "abc123", "ABC12-", "ABC 12", "ABC12é",
    ] {
        assert_eq!(
            JoinCode::new(bad),
            Err(ConfigError::InvalidJoinCode),
            "{bad:?} must be rejected"
        );
    }
}
