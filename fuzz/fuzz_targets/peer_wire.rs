//! The peer-wire message parsers never panic on what a peer sends: the
//! BEP 3 handshake, the BEP 10 extended handshake, BEP 9 `ut_metadata`
//! messages and BEP 11 `ut_pex`. What parses keeps its bounds, and
//! handshakes and peer lists survive our own encoders. Message bodies only:
//! framing and MSE need the other side of a connection.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_peer_wire::message::{
    HANDSHAKE_LEN, MetadataMessage, handshake, parse_extended_handshake, parse_handshake,
    parse_metadata_message, parse_pex, pex_message,
};

fuzz_target!(|data: &[u8]| {
    if let Some(Ok(h)) = data.first_chunk::<HANDSHAKE_LEN>().map(parse_handshake) {
        // What we read is what we'd write for the same peer.
        let ours = handshake(&h.info_hash, &h.peer_id, h.extensions);
        assert_eq!(parse_handshake(&ours).ok(), Some(h));
    }
    if let Ok(h) = parse_extended_handshake(data) {
        // 0 means "disabled" and is never kept as an id.
        assert!(h.ut_metadata != Some(0) && h.ut_pex != Some(0));
        // The client name is cut to 64 bytes before it's decoded.
        assert!(h.client.is_none_or(|c| c.chars().count() <= 64));
    }
    if let Ok(MetadataMessage::Data { data: piece, .. }) = parse_metadata_message(data) {
        // The piece is what follows the header in this message, nothing else.
        assert!(piece.len() < data.len());
        assert!(piece.as_ptr_range().end == data.as_ptr_range().end);
    }
    let peers = parse_pex(data);
    // Each peer took at least six bytes of the message.
    assert!(peers.len() <= data.len() / 6);
    assert_eq!(parse_pex(&pex_message(&peers)), peers);
});
