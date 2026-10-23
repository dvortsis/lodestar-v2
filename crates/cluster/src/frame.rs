//! Frames: how a protocol message travels (§19.2).
//!
//! A frame is one WebSocket binary message (or, on a join connection, a
//! big-endian `u32` length and then the frame):
//!
//! ```text
//! byte 0     format: 0 = postcard, 1 = zstd-compressed postcard
//! bytes 1..  the message
//! ```
//!
//! Messages of [`COMPRESS_FROM`] bytes or more (the batches) are
//! compressed when that makes them smaller.
//!
//! Decoding is strict and bounded, because the bytes come from the network:
//! - a frame is at most [`MAX_FRAME`] bytes on the wire, and its message at
//!   most [`MAX_MESSAGE`] bytes once decompressed;
//! - zstd may use at most a `2^`[`MAX_WINDOW_LOG`]-byte window (a frame
//!   can't make the decoder allocate a large one), must be a single frame,
//!   and decompression stops one byte past the cap;
//! - the message must use every byte (no trailing data) and pass its
//!   `validate` limits.
//!
//! Nothing here panics on any input (property-tested, and the
//! `cluster_frame` fuzz target).

use std::io::Read;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::proto::{JoinReply, JoinRequest, ToCoordinator, ToWorker};

/// A frame's largest size on the wire.
pub const MAX_FRAME: usize = 1 << 20;
/// A message's largest size once decompressed.
pub const MAX_MESSAGE: usize = 1 << 20;
pub const MAX_WINDOW_LOG: u32 = 20;
pub const COMPRESS_FROM: usize = 1024;
/// A join request (a token and a key) is small; its frame is capped far
/// below a cluster frame, since it arrives before any authentication.
pub const MAX_JOIN_REQUEST: usize = 1024;
/// A join reply carries a few certificates.
pub const MAX_JOIN_REPLY: usize = 64 * 1024;

const PLAIN: u8 = 0;
const ZSTD: u8 = 1;
const ZSTD_LEVEL: i32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    Empty,
    /// Over [`MAX_FRAME`] or [`MAX_MESSAGE`].
    TooLarge,
    UnknownFormat(u8),
    Decompress,
    /// Bytes after the message, or after the zstd frame.
    Trailing,
    Decode,
    /// The message breaks one of its limits.
    Invalid,
    Encode,
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::Empty => f.write_str("empty frame"),
            FrameError::TooLarge => f.write_str("frame over the size limit"),
            FrameError::UnknownFormat(b) => write!(f, "unknown frame format {b}"),
            FrameError::Decompress => f.write_str("bad compressed frame"),
            FrameError::Trailing => f.write_str("trailing bytes in a frame"),
            FrameError::Decode => f.write_str("malformed message"),
            FrameError::Invalid => f.write_str("message over a protocol limit"),
            FrameError::Encode => f.write_str("message can't be encoded"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Encodes a message, compressing it if it's large. A message over the
/// limits is a bug on this side, refused rather than sent.
pub fn encode<T: Serialize>(msg: &T) -> Result<Vec<u8>, FrameError> {
    let body = postcard::to_allocvec(msg).map_err(|_| FrameError::Encode)?;
    if body.len() > MAX_MESSAGE {
        return Err(FrameError::TooLarge);
    }
    if body.len() >= COMPRESS_FROM {
        match zstd::bulk::compress(&body, ZSTD_LEVEL) {
            Ok(packed) if packed.len() < body.len() => return framed(ZSTD, &packed),
            _ => {}
        }
    }
    framed(PLAIN, &body)
}

fn framed(format: u8, body: &[u8]) -> Result<Vec<u8>, FrameError> {
    if body.len() + 1 > MAX_FRAME {
        return Err(FrameError::TooLarge);
    }
    let mut out = Vec::with_capacity(body.len() + 1);
    out.push(format);
    out.extend_from_slice(body);
    Ok(out)
}

/// Decodes one frame into a message, without checking its limits.
pub fn decode<T: DeserializeOwned>(frame: &[u8]) -> Result<T, FrameError> {
    let (&format, body) = frame.split_first().ok_or(FrameError::Empty)?;
    if frame.len() > MAX_FRAME {
        return Err(FrameError::TooLarge);
    }
    match format {
        PLAIN => strict(body),
        ZSTD => strict(&decompress(body)?),
        other => Err(FrameError::UnknownFormat(other)),
    }
}

fn strict<T: DeserializeOwned>(body: &[u8]) -> Result<T, FrameError> {
    if body.len() > MAX_MESSAGE {
        return Err(FrameError::TooLarge);
    }
    match postcard::take_from_bytes(body) {
        Ok((msg, [])) => Ok(msg),
        Ok(_) => Err(FrameError::Trailing),
        Err(_) => Err(FrameError::Decode),
    }
}

fn decompress(body: &[u8]) -> Result<Vec<u8>, FrameError> {
    let mut decoder = zstd::stream::read::Decoder::with_buffer(body)
        .map_err(|_| FrameError::Decompress)?
        .single_frame();
    decoder
        .window_log_max(MAX_WINDOW_LOG)
        .map_err(|_| FrameError::Decompress)?;
    let mut out = Vec::new();
    (&mut decoder)
        .take(MAX_MESSAGE as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|_| FrameError::Decompress)?;
    if out.len() > MAX_MESSAGE {
        return Err(FrameError::TooLarge);
    }
    if !decoder.finish().is_empty() {
        return Err(FrameError::Trailing);
    }
    Ok(out)
}

/// A worker's message, as the coordinator reads it.
pub fn decode_to_coordinator(frame: &[u8]) -> Result<ToCoordinator, FrameError> {
    let msg: ToCoordinator = decode(frame)?;
    msg.validate().then_some(msg).ok_or(FrameError::Invalid)
}

/// The coordinator's message, as a worker reads it.
pub fn decode_to_worker(frame: &[u8]) -> Result<ToWorker, FrameError> {
    let msg: ToWorker = decode(frame)?;
    msg.validate().then_some(msg).ok_or(FrameError::Invalid)
}

pub fn decode_join_request(frame: &[u8]) -> Result<JoinRequest, FrameError> {
    if frame.len() > MAX_JOIN_REQUEST {
        return Err(FrameError::TooLarge);
    }
    let req: JoinRequest = decode(frame)?;
    // A token is 126 characters; anything much longer isn't one.
    (req.token.len() <= 256)
        .then_some(req)
        .ok_or(FrameError::Invalid)
}

pub fn decode_join_reply(frame: &[u8]) -> Result<JoinReply, FrameError> {
    if frame.len() > MAX_JOIN_REPLY {
        return Err(FrameError::TooLarge);
    }
    let reply: JoinReply = decode(frame)?;
    reply.validate().then_some(reply).ok_or(FrameError::Invalid)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::proto::*;

    fn claim(n: usize) -> ToCoordinator {
        ToCoordinator::Claim(ClaimBatch {
            batch: 7,
            items: (0..n)
                .map(|i| ClaimItem {
                    key: [(i % 251) as u8; 20],
                    source: 3,
                    strategy: 1,
                    discovered_ms: 1_800_000_000_000 + i as i64,
                })
                .collect(),
        })
    }

    #[test]
    fn small_messages_are_plain_and_batches_compressed() {
        let hb = ToCoordinator::Heartbeat(Heartbeat::default());
        let frame = encode(&hb).unwrap();
        assert_eq!(frame[0], PLAIN);
        assert_eq!(decode_to_coordinator(&frame).unwrap(), hb);

        let batch = claim(MAX_CLAIM_BATCH);
        let frame = encode(&batch).unwrap();
        assert_eq!(frame[0], ZSTD);
        assert!(frame.len() < postcard::to_allocvec(&batch).unwrap().len() / 2);
        assert_eq!(decode_to_coordinator(&frame).unwrap(), batch);
    }

    #[test]
    fn limits_are_checked_after_decoding() {
        let frame = encode(&claim(MAX_CLAIM_BATCH + 1)).unwrap();
        assert!(decode::<ToCoordinator>(&frame).is_ok());
        assert_eq!(decode_to_coordinator(&frame), Err(FrameError::Invalid));
        let empty = encode(&ToCoordinator::Claim(ClaimBatch {
            batch: 1,
            items: vec![],
        }))
        .unwrap();
        assert_eq!(decode_to_coordinator(&empty), Err(FrameError::Invalid));
        let zero = encode(&claim(1)).unwrap();
        let mut msg: ToCoordinator = decode(&zero).unwrap();
        if let ToCoordinator::Claim(c) = &mut msg {
            c.batch = 0;
        }
        assert_eq!(
            decode_to_coordinator(&encode(&msg).unwrap()),
            Err(FrameError::Invalid),
            "batch 0 is the coordinator's"
        );
    }

    #[test]
    fn malformed_frames_are_refused() {
        assert_eq!(decode::<ToWorker>(&[]), Err(FrameError::Empty));
        assert_eq!(
            decode::<ToWorker>(&[9, 0]),
            Err(FrameError::UnknownFormat(9))
        );
        assert_eq!(decode::<ToWorker>(&[PLAIN]), Err(FrameError::Decode));
        let mut frame = encode(&ToWorker::Drain).unwrap();
        frame.push(0);
        assert_eq!(decode::<ToWorker>(&frame), Err(FrameError::Trailing));
        assert_eq!(
            decode::<ToWorker>(&[ZSTD, 1, 2, 3]),
            Err(FrameError::Decompress)
        );
        let mut big = vec![PLAIN];
        big.resize(MAX_FRAME + 1, 0);
        assert_eq!(decode::<ToWorker>(&big), Err(FrameError::TooLarge));
    }

    #[test]
    fn a_vector_claiming_a_huge_length_fails_without_allocating_it() {
        // Claim, batch 1, then a varint item count of 2^62.
        let mut frame = vec![PLAIN, 2, 1];
        frame.extend_from_slice(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x40]);
        assert_eq!(decode::<ToCoordinator>(&frame), Err(FrameError::Decode));
    }

    #[test]
    fn a_decompression_bomb_stops_at_the_cap() {
        let zeros = vec![0u8; 8 * MAX_MESSAGE];
        // Within the window limit, so only the output cap stops it.
        let mut c = zstd::bulk::Compressor::new(19).unwrap();
        c.set_parameter(zstd::zstd_safe::CParameter::WindowLog(MAX_WINDOW_LOG))
            .unwrap();
        let bomb = c.compress(&zeros).unwrap();
        assert!(bomb.len() < 4096);
        let mut frame = vec![ZSTD];
        frame.extend_from_slice(&bomb);
        assert_eq!(decode::<ToWorker>(&frame), Err(FrameError::TooLarge));
    }

    #[test]
    fn a_frame_asking_for_a_large_window_is_refused() {
        let data: Vec<u8> = (0..(4 << 20))
            .map(|i: u32| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
            .collect();
        // Streamed without a content size, so the window is what it says.
        let mut packed = Vec::new();
        {
            let mut enc = zstd::stream::write::Encoder::new(&mut packed, 3).unwrap();
            enc.set_parameter(zstd::zstd_safe::CParameter::WindowLog(23))
                .unwrap();
            std::io::Write::write_all(&mut enc, &data).unwrap();
            enc.finish().unwrap();
        }
        let mut frame = vec![ZSTD];
        frame.extend_from_slice(&packed[..packed.len().min(MAX_FRAME - 1)]);
        assert_eq!(decode::<ToWorker>(&frame), Err(FrameError::Decompress));
    }

    #[test]
    fn two_zstd_frames_are_refused() {
        let body = postcard::to_allocvec(&ToWorker::Drain).unwrap();
        let one = zstd::bulk::compress(&body, 3).unwrap();
        let mut frame = vec![ZSTD];
        frame.extend_from_slice(&one);
        assert_eq!(decode::<ToWorker>(&frame), Ok(ToWorker::Drain));
        frame.extend_from_slice(&one);
        assert_eq!(decode::<ToWorker>(&frame), Err(FrameError::Trailing));
    }

    #[test]
    fn every_message_within_its_limits_fits_in_a_frame() {
        // Incompressible bytes, so the frame is as large as it gets.
        let noise = |n: usize| -> Vec<u8> {
            (0..n as u32)
                .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
                .collect()
        };
        let mut key = 0u32;
        let mut next_key = || {
            key += 1;
            let mut k = [0u8; 20];
            k[..4].copy_from_slice(&key.wrapping_mul(2_654_435_761).to_le_bytes());
            k
        };
        let grant = WireGrant {
            worker: u32::MAX,
            name: "a".repeat(63),
            cert: noise(MAX_CERT),
            not_after: i64::MAX,
            cas: vec![noise(MAX_CERT); MAX_CAS],
        };
        let to_worker = [
            ToWorker::Grants(GrantBatch {
                batch: u64::MAX,
                items: (0..MAX_GRANT_BATCH)
                    .map(|_| GrantItem::Granted {
                        key: next_key(),
                        token: u64::MAX,
                        ttl_secs: u32::MAX,
                    })
                    .collect(),
            }),
            ToWorker::Renewed(Box::new(grant.clone())),
            ToWorker::Assign(Assign {
                revision: u64::MAX,
                payload: noise(MAX_OPAQUE),
            }),
            ToWorker::HaveSnapshot(noise(MAX_OPAQUE)),
        ];
        for msg in to_worker {
            assert!(msg.validate());
            assert_eq!(decode_to_worker(&encode(&msg).unwrap()).unwrap(), msg);
        }
        let to_coordinator = [
            ToCoordinator::Claim(ClaimBatch {
                batch: u64::MAX,
                items: (0..MAX_CLAIM_BATCH)
                    .map(|_| ClaimItem {
                        key: next_key(),
                        source: 63,
                        strategy: u8::MAX,
                        discovered_ms: i64::MIN,
                    })
                    .collect(),
            }),
            ToCoordinator::Outcome(OutcomeBatch {
                items: (0..MAX_OUTCOME_BATCH)
                    .map(|_| OutcomeItem {
                        key: next_key(),
                        token: u64::MAX,
                        outcome: Outcome::Lost { reason: u8::MAX },
                    })
                    .collect(),
            }),
            ToCoordinator::Results(noise(MAX_OPAQUE)),
        ];
        for msg in to_coordinator {
            assert!(msg.validate());
            assert_eq!(decode_to_coordinator(&encode(&msg).unwrap()).unwrap(), msg);
        }
        let reply = JoinReply::Granted(Box::new(grant));
        assert_eq!(decode_join_reply(&encode(&reply).unwrap()).unwrap(), reply);
    }

    #[test]
    fn oversized_messages_are_not_sent() {
        let msg = ToWorker::HaveSnapshot(vec![7; MAX_MESSAGE]);
        assert_eq!(encode(&msg), Err(FrameError::TooLarge));
    }

    #[test]
    fn join_frames_have_their_own_caps() {
        let req = JoinRequest {
            major: PROTOCOL_MAJOR,
            token: "lsj1".into(),
            public_key: [1; 32],
        };
        assert_eq!(decode_join_request(&encode(&req).unwrap()).unwrap(), req);
        let long = JoinRequest {
            token: "x".repeat(300),
            ..req
        };
        assert_eq!(
            decode_join_request(&encode(&long).unwrap()),
            Err(FrameError::Invalid)
        );
        let mut big = encode(&ToWorker::Drain).unwrap();
        big.resize(MAX_JOIN_REQUEST + 1, 0);
        assert_eq!(decode_join_request(&big), Err(FrameError::TooLarge));
        assert_eq!(
            decode_join_reply(&encode(&JoinReply::Refused).unwrap()),
            Ok(JoinReply::Refused)
        );
    }

    fn any_key() -> impl Strategy<Value = [u8; 20]> {
        any::<[u8; 20]>()
    }

    fn any_to_coordinator() -> impl Strategy<Value = ToCoordinator> {
        let item = (any_key(), any::<u8>(), any::<u8>(), any::<i64>()).prop_map(
            |(key, source, strategy, discovered_ms)| ClaimItem {
                key,
                source,
                strategy,
                discovered_ms,
            },
        );
        let outcome =
            (any_key(), any::<u64>(), 0u8..3, any::<u8>()).prop_map(|(key, token, k, r)| {
                OutcomeItem {
                    key,
                    token,
                    outcome: match k {
                        0 => Outcome::Fetched,
                        1 => Outcome::PeerFound { reason: r },
                        _ => Outcome::Lost { reason: r },
                    },
                }
            });
        prop_oneof![
            (
                any::<u16>(),
                any::<u16>(),
                any::<u32>(),
                any::<u64>(),
                proptest::collection::vec(any::<[u8; 4]>(), 0..4)
            )
                .prop_map(|(major, minor, worker, companions, ips)| {
                    ToCoordinator::Hello(Hello {
                        major,
                        minor,
                        worker,
                        capabilities: Capabilities {
                            cores: 4,
                            ram_mb: 1024,
                            external_ips: ips.into_iter().map(std::net::IpAddr::from).collect(),
                            companions,
                            egress: 1,
                        },
                    })
                }),
            (
                any::<u64>(),
                any::<i64>(),
                any::<Option<(u64, i64)>>(),
                any::<u32>()
            )
                .prop_map(|(seq, sent_ms, ack, leases)| ToCoordinator::Heartbeat(
                    Heartbeat {
                        seq,
                        sent_ms,
                        last_ack: ack.map(|(seq, received_ms)| AckEcho { seq, received_ms }),
                        leases,
                        stats: Stats::default(),
                    }
                )),
            (1u64.., proptest::collection::vec(item, 1..600))
                .prop_map(|(batch, items)| ToCoordinator::Claim(ClaimBatch { batch, items })),
            proptest::collection::vec(outcome, 1..100)
                .prop_map(|items| ToCoordinator::Outcome(OutcomeBatch { items })),
            any::<[u8; 32]>().prop_map(|public_key| ToCoordinator::Renew { public_key }),
            Just(ToCoordinator::Drained),
            proptest::collection::vec(any::<u8>(), 0..2000).prop_map(ToCoordinator::Results),
        ]
    }

    proptest! {
        #[test]
        fn messages_round_trip(msg in any_to_coordinator()) {
            let frame = encode(&msg).unwrap();
            prop_assert!(frame.len() <= MAX_FRAME);
            let back: ToCoordinator = decode(&frame).unwrap();
            prop_assert_eq!(&back, &msg);
            prop_assert_eq!(decode_to_coordinator(&frame).is_ok(), msg.validate());
        }

        #[test]
        fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
            let _ = decode_to_coordinator(&bytes);
            let _ = decode_to_worker(&bytes);
            let _ = decode_join_request(&bytes);
            let _ = decode_join_reply(&bytes);
        }

        #[test]
        fn mutated_frames_never_panic(msg in any_to_coordinator(), flips in proptest::collection::vec((any::<usize>(), any::<u8>()), 1..8)) {
            let mut frame = encode(&msg).unwrap();
            for (at, byte) in flips {
                let i = at % frame.len();
                frame[i] ^= byte | 1;
            }
            if let Ok(m) = decode_to_coordinator(&frame) {
                // Whatever decodes is a valid message that encodes again.
                prop_assert!(m.validate());
                prop_assert!(encode(&m).is_ok());
            }
        }
    }
}
