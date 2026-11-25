#![no_main]

use libfuzzer_sys::fuzz_target;
use superminigames_protocol::{decode_client, decode_server, encode_client, encode_server};

// VER-21: arbitrary binary frames must never panic; valid frames round-trip,
// and the codec rejects every frame above PRO-10's fixed size limit. The fuzz
// input is the only source of bytes; no clock or OS randomness is used.
fuzz_target!(|data: &[u8]| {
    if let Ok(client) = decode_client(data) {
        let encoded = encode_client(&client).expect("a decoded client frame re-encodes");
        let decoded = decode_client(&encoded).expect("the re-encoded client frame decodes");
        assert_eq!(client, decoded, "VER-21: client frame round-trips");
    }

    if let Ok(server) = decode_server(data) {
        let encoded = encode_server(&server).expect("a decoded server frame re-encodes");
        let decoded = decode_server(&encoded).expect("the re-encoded server frame decodes");
        assert_eq!(server, decoded, "VER-21: server frame round-trips");
    }

    if data.len() > superminigames_protocol::MAX_MESSAGE_BYTES {
        assert!(
            decode_client(data).is_err(),
            "PRO-10: client oversize rejected"
        );
        assert!(
            decode_server(data).is_err(),
            "PRO-10: server oversize rejected"
        );
    }
});
