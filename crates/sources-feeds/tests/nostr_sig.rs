//! BIP-340 Schnorr checks for Nostr events (NOS-1): BIP-340's official
//! test vectors as a golden test, edge cases a hostile relay could send,
//! property tests that nothing panics and that any flipped bit fails, and
//! synthetic signed events through `parse_signed`.
//!
//! Every key here is a test key: the BIP-340 vectors' own, or a fixed
//! test secret. No real key and no real relay data.

use k256::schnorr::SigningKey;
use lodestar_sources_feeds::nostr::{self, KIND_TORRENT, LIMITS, Message, Refused, event_id};
use lodestar_sources_feeds::nostr_sig::{Rejected, event_signed, parse_signed, verify};
use proptest::prelude::*;

/// One row of BIP-340's `test-vectors.csv`.
struct V {
    index: u8,
    /// Empty for the verification-only rows (4–14).
    secret: &'static str,
    pubkey: &'static str,
    /// Empty for the verification-only rows.
    aux: &'static str,
    /// Any length: rows 15–18 sign 0, 1, 17 and 100 bytes.
    msg: &'static str,
    sig: &'static str,
    valid: bool,
    comment: &'static str,
}

/// BIP-340's test vectors, all 19 rows (0–18), transcribed from
/// `bip-0340/test-vectors.csv` in `github.com/bitcoin/bips` at commit
/// fdcd8024595bc1e97db73883e716dbb91627c371 (master on 2026-11-23Z; the
/// file's blob is 672339129a844a060591bb22f444158ff45438ed, SHA-256
/// 34c9d1d9c3a88d524bc80778540dc43f8306ec249a7485293063c376db851c2d).
/// BIP-340 is licensed BSD-2-Clause (its reference code BSD-2-Clause OR
/// MIT OR CC0-1.0); these are its published test data, reproduced
/// unchanged.
const VECTORS: &[V] = &[
    V {
        index: 0,
        secret: "0000000000000000000000000000000000000000000000000000000000000003",
        pubkey: "F9308A019258C31049344F85F89D5229B531C845836F99B08601F113BCE036F9",
        aux: "0000000000000000000000000000000000000000000000000000000000000000",
        msg: "0000000000000000000000000000000000000000000000000000000000000000",
        sig: "E907831F80848D1069A5371B402410364BDF1C5F8307B0084C55F1CE2DCA8215\
              25F66A4A85EA8B71E482A74F382D2CE5EBEEE8FDB2172F477DF4900D310536C0",
        valid: true,
        comment: "",
    },
    V {
        index: 1,
        secret: "B7E151628AED2A6ABF7158809CF4F3C762E7160F38B4DA56A784D9045190CFEF",
        pubkey: "DFF1D77F2A671C5F36183726DB2341BE58FEAE1DA2DECED843240F7B502BA659",
        aux: "0000000000000000000000000000000000000000000000000000000000000001",
        msg: "243F6A8885A308D313198A2E03707344A4093822299F31D0082EFA98EC4E6C89",
        sig: "6896BD60EEAE296DB48A229FF71DFE071BDE413E6D43F917DC8DCF8C78DE3341\
              8906D11AC976ABCCB20B091292BFF4EA897EFCB639EA871CFA95F6DE339E4B0A",
        valid: true,
        comment: "",
    },
    V {
        index: 2,
        secret: "C90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74020BBEA63B14E5C9",
        pubkey: "DD308AFEC5777E13121FA72B9CC1B7CC0139715309B086C960E18FD969774EB8",
        aux: "C87AA53824B4D7AE2EB035A2B5BBBCCC080E76CDC6D1692C4B0B62D798E6D906",
        msg: "7E2D58D8B3BCDF1ABADEC7829054F90DDA9805AAB56C77333024B9D0A508B75C",
        sig: "5831AAEED7B44BB74E5EAB94BA9D4294C49BCF2A60728D8B4C200F50DD313C1B\
              AB745879A5AD954A72C45A91C3A51D3C7ADEA98D82F8481E0E1E03674A6F3FB7",
        valid: true,
        comment: "",
    },
    V {
        index: 3,
        secret: "0B432B2677937381AEF05BB02A66ECD012773062CF3FA2549E44F58ED2401710",
        pubkey: "25D1DFF95105F5253C4022F628A996AD3A0D95FBF21D468A1B33F8C160D8F517",
        aux: "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF",
        msg: "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF",
        sig: "7EB0509757E246F19449885651611CB965ECC1A187DD51B64FDA1EDC9637D5EC\
              97582B9CB13DB3933705B32BA982AF5AF25FD78881EBB32771FC5922EFC66EA3",
        valid: true,
        comment: "test fails if msg is reduced modulo p or n",
    },
    V {
        index: 4,
        secret: "",
        pubkey: "D69C3509BB99E412E68B0FE8544E72837DFA30746D8BE2AA65975F29D22DC7B9",
        aux: "",
        msg: "4DF3C3F68FCC83B27E9D42C90431A72499F17875C81A599B566C9889B9696703",
        sig: "00000000000000000000003B78CE563F89A0ED9414F5AA28AD0D96D6795F9C63\
              76AFB1548AF603B3EB45C9F8207DEE1060CB71C04E80F593060B07D28308D7F4",
        valid: true,
        comment: "",
    },
    V {
        index: 5,
        secret: "",
        pubkey: "EEFDEA4CDB677750A420FEE807EACF21EB9898AE79B9768766E4FAA04A2D4A34",
        aux: "",
        msg: "243F6A8885A308D313198A2E03707344A4093822299F31D0082EFA98EC4E6C89",
        sig: "6CFF5C3BA86C69EA4B7376F31A9BCB4F74C1976089B2D9963DA2E5543E177769\
              69E89B4C5564D00349106B8497785DD7D1D713A8AE82B32FA79D5F7FC407D39B",
        valid: false,
        comment: "public key not on the curve",
    },
    V {
        index: 6,
        secret: "",
        pubkey: "DFF1D77F2A671C5F36183726DB2341BE58FEAE1DA2DECED843240F7B502BA659",
        aux: "",
        msg: "243F6A8885A308D313198A2E03707344A4093822299F31D0082EFA98EC4E6C89",
        sig: "FFF97BD5755EEEA420453A14355235D382F6472F8568A18B2F057A1460297556\
              3CC27944640AC607CD107AE10923D9EF7A73C643E166BE5EBEAFA34B1AC553E2",
        valid: false,
        comment: "has_even_y(R) is false",
    },
    V {
        index: 7,
        secret: "",
        pubkey: "DFF1D77F2A671C5F36183726DB2341BE58FEAE1DA2DECED843240F7B502BA659",
        aux: "",
        msg: "243F6A8885A308D313198A2E03707344A4093822299F31D0082EFA98EC4E6C89",
        sig: "1FA62E331EDBC21C394792D2AB1100A7B432B013DF3F6FF4F99FCB33E0E1515F\
              28890B3EDB6E7189B630448B515CE4F8622A954CFE545735AAEA5134FCCDB2BD",
        valid: false,
        comment: "negated message",
    },
    V {
        index: 8,
        secret: "",
        pubkey: "DFF1D77F2A671C5F36183726DB2341BE58FEAE1DA2DECED843240F7B502BA659",
        aux: "",
        msg: "243F6A8885A308D313198A2E03707344A4093822299F31D0082EFA98EC4E6C89",
        sig: "6CFF5C3BA86C69EA4B7376F31A9BCB4F74C1976089B2D9963DA2E5543E177769\
              961764B3AA9B2FFCB6EF947B6887A226E8D7C93E00C5ED0C1834FF0D0C2E6DA6",
        valid: false,
        comment: "negated s value",
    },
    V {
        index: 9,
        secret: "",
        pubkey: "DFF1D77F2A671C5F36183726DB2341BE58FEAE1DA2DECED843240F7B502BA659",
        aux: "",
        msg: "243F6A8885A308D313198A2E03707344A4093822299F31D0082EFA98EC4E6C89",
        sig: "0000000000000000000000000000000000000000000000000000000000000000\
              123DDA8328AF9C23A94C1FEECFD123BA4FB73476F0D594DCB65C6425BD186051",
        valid: false,
        comment: "sG - eP is infinite. Test fails in single verification if has_even_y(inf) is defined as true and x(inf) as 0",
    },
    V {
        index: 10,
        secret: "",
        pubkey: "DFF1D77F2A671C5F36183726DB2341BE58FEAE1DA2DECED843240F7B502BA659",
        aux: "",
        msg: "243F6A8885A308D313198A2E03707344A4093822299F31D0082EFA98EC4E6C89",
        sig: "0000000000000000000000000000000000000000000000000000000000000001\
              7615FBAF5AE28864013C099742DEADB4DBA87F11AC6754F93780D5A1837CF197",
        valid: false,
        comment: "sG - eP is infinite. Test fails in single verification if has_even_y(inf) is defined as true and x(inf) as 1",
    },
    V {
        index: 11,
        secret: "",
        pubkey: "DFF1D77F2A671C5F36183726DB2341BE58FEAE1DA2DECED843240F7B502BA659",
        aux: "",
        msg: "243F6A8885A308D313198A2E03707344A4093822299F31D0082EFA98EC4E6C89",
        sig: "4A298DACAE57395A15D0795DDBFD1DCB564DA82B0F269BC70A74F8220429BA1D\
              69E89B4C5564D00349106B8497785DD7D1D713A8AE82B32FA79D5F7FC407D39B",
        valid: false,
        comment: "sig[0:32] is not an X coordinate on the curve",
    },
    V {
        index: 12,
        secret: "",
        pubkey: "DFF1D77F2A671C5F36183726DB2341BE58FEAE1DA2DECED843240F7B502BA659",
        aux: "",
        msg: "243F6A8885A308D313198A2E03707344A4093822299F31D0082EFA98EC4E6C89",
        sig: "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFC2F\
              69E89B4C5564D00349106B8497785DD7D1D713A8AE82B32FA79D5F7FC407D39B",
        valid: false,
        comment: "sig[0:32] is equal to field size",
    },
    V {
        index: 13,
        secret: "",
        pubkey: "DFF1D77F2A671C5F36183726DB2341BE58FEAE1DA2DECED843240F7B502BA659",
        aux: "",
        msg: "243F6A8885A308D313198A2E03707344A4093822299F31D0082EFA98EC4E6C89",
        sig: "6CFF5C3BA86C69EA4B7376F31A9BCB4F74C1976089B2D9963DA2E5543E177769\
              FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141",
        valid: false,
        comment: "sig[32:64] is equal to curve order",
    },
    V {
        index: 14,
        secret: "",
        pubkey: "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFC30",
        aux: "",
        msg: "243F6A8885A308D313198A2E03707344A4093822299F31D0082EFA98EC4E6C89",
        sig: "6CFF5C3BA86C69EA4B7376F31A9BCB4F74C1976089B2D9963DA2E5543E177769\
              69E89B4C5564D00349106B8497785DD7D1D713A8AE82B32FA79D5F7FC407D39B",
        valid: false,
        comment: "public key is not a valid X coordinate because it exceeds the field size",
    },
    V {
        index: 15,
        secret: "0340034003400340034003400340034003400340034003400340034003400340",
        pubkey: "778CAA53B4393AC467774D09497A87224BF9FAB6F6E68B23086497324D6FD117",
        aux: "0000000000000000000000000000000000000000000000000000000000000000",
        msg: "",
        sig: "71535DB165ECD9FBBC046E5FFAEA61186BB6AD436732FCCC25291A55895464CF\
              6069CE26BF03466228F19A3A62DB8A649F2D560FAC652827D1AF0574E427AB63",
        valid: true,
        comment: "message of size 0 (added 2022-12)",
    },
    V {
        index: 16,
        secret: "0340034003400340034003400340034003400340034003400340034003400340",
        pubkey: "778CAA53B4393AC467774D09497A87224BF9FAB6F6E68B23086497324D6FD117",
        aux: "0000000000000000000000000000000000000000000000000000000000000000",
        msg: "11",
        sig: "08A20A0AFEF64124649232E0693C583AB1B9934AE63B4C3511F3AE1134C6A303\
              EA3173BFEA6683BD101FA5AA5DBC1996FE7CACFC5A577D33EC14564CEC2BACBF",
        valid: true,
        comment: "message of size 1 (added 2022-12)",
    },
    V {
        index: 17,
        secret: "0340034003400340034003400340034003400340034003400340034003400340",
        pubkey: "778CAA53B4393AC467774D09497A87224BF9FAB6F6E68B23086497324D6FD117",
        aux: "0000000000000000000000000000000000000000000000000000000000000000",
        msg: "0102030405060708090A0B0C0D0E0F1011",
        sig: "5130F39A4059B43BC7CAC09A19ECE52B5D8699D1A71E3C52DA9AFDB6B50AC370\
              C4A482B77BF960F8681540E25B6771ECE1E5A37FD80E5A51897C5566A97EA5A5",
        valid: true,
        comment: "message of size 17 (added 2022-12)",
    },
    V {
        index: 18,
        secret: "0340034003400340034003400340034003400340034003400340034003400340",
        pubkey: "778CAA53B4393AC467774D09497A87224BF9FAB6F6E68B23086497324D6FD117",
        aux: "0000000000000000000000000000000000000000000000000000000000000000",
        msg: "99999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999",
        sig: "403B12B0D8555A344175EA7EC746566303321E5DBFA8BE6F091635163ECA79A8\
              585ED3E3170807E7C03B720FC54C7B23897FCBA0E9D0B4A06894CFD249F22367",
        valid: true,
        comment: "message of size 100 (added 2022-12)",
    },
];

/// Hex of either case, as the vectors write it.
fn unhex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "odd hex: {s}");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn arr<const N: usize>(s: &str) -> [u8; N] {
    unhex(s).try_into().unwrap()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[test]
fn bip340_vectors_all_rows() {
    let idx: Vec<u8> = VECTORS.iter().map(|v| v.index).collect();
    assert_eq!(idx, (0..=18).collect::<Vec<u8>>());
    assert_eq!(VECTORS.iter().filter(|v| !v.valid).count(), 10);
}

#[test]
fn bip340_verification_vectors() {
    for v in VECTORS {
        let got = verify(&arr(v.pubkey), &unhex(v.msg), &arr(v.sig));
        assert_eq!(got, v.valid, "vector {} ({})", v.index, v.comment);
    }
}

/// The signer the synthetic events below use is the vectors' too: each
/// row with a secret key gives its public key and, with its aux_rand, its
/// exact signature.
#[test]
fn bip340_signing_vectors() {
    let mut n = 0;
    for v in VECTORS.iter().filter(|v| !v.secret.is_empty()) {
        let sk = SigningKey::from_slice(&unhex(v.secret)).unwrap();
        assert_eq!(
            sk.verifying_key().to_bytes().as_slice(),
            unhex(v.pubkey).as_slice(),
            "vector {}",
            v.index
        );
        let sig = sk.sign_raw(&unhex(v.msg), &arr(v.aux)).unwrap();
        assert_eq!(
            sig.to_bytes().as_slice(),
            unhex(v.sig).as_slice(),
            "vector {}",
            v.index
        );
        n += 1;
    }
    assert_eq!(n, 8);
}

/// secp256k1's field size p and group order n.
const P: &str = "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFC2F";
const N: &str = "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141";

/// Vector 1's key, message and signature: the valid baseline the edge
/// cases change one part of.
fn baseline() -> ([u8; 32], Vec<u8>, [u8; 64]) {
    let v = &VECTORS[1];
    (arr(v.pubkey), unhex(v.msg), arr(v.sig))
}

fn with_r(sig: &[u8; 64], r: &[u8]) -> [u8; 64] {
    let mut s = *sig;
    s[..32].copy_from_slice(r);
    s
}

fn with_s(sig: &[u8; 64], v: &[u8]) -> [u8; 64] {
    let mut s = *sig;
    s[32..].copy_from_slice(v);
    s
}

/// Values a hostile relay could send that BIP-340 rejects, each a plain
/// false: no panic, no error escapes.
#[test]
fn hostile_values_fail() {
    let (pk, msg, sig) = baseline();
    assert!(verify(&pk, &msg, &sig));
    let ff = [0xff; 32];
    let zero = [0u8; 32];
    let mut p_plus_1 = arr::<32>(P);
    p_plus_1[31] += 1;
    let mut n_plus_1 = arr::<32>(N);
    n_plus_1[31] += 1;
    for (what, pk2, sig2) in [
        ("r = p", pk, with_r(&sig, &unhex(P))),
        ("r = p + 1", pk, with_r(&sig, &p_plus_1)),
        ("r = 2^256 - 1", pk, with_r(&sig, &ff)),
        ("r = 0", pk, with_r(&sig, &zero)),
        ("s = n", pk, with_s(&sig, &unhex(N))),
        ("s = n + 1", pk, with_s(&sig, &n_plus_1)),
        ("s = 2^256 - 1", pk, with_s(&sig, &ff)),
        ("s = 0", pk, with_s(&sig, &zero)),
        ("all-zero signature", pk, [0u8; 64]),
        ("all-ones signature", pk, [0xff; 64]),
        ("key = 0", zero, sig),
        ("key = p", arr(P), sig),
        ("key = 2^256 - 1", ff, sig),
        // x = 5 has no y on secp256k1 (5^3 + 7 = 132 is not a square
        // mod p), the same failure as vector 5.
        (
            "key not on the curve",
            arr::<32>(&format!("{:064x}", 5)),
            sig,
        ),
    ] {
        assert!(!verify(&pk2, &msg, &sig2), "{what}");
    }
}

/// A fixed test secret (bytes 1 to 32); never a real key.
fn test_key() -> SigningKey {
    let secret: Vec<u8> = (1..=32).collect();
    SigningKey::from_slice(&secret).unwrap()
}

fn sign(sk: &SigningKey, msg: &[u8]) -> [u8; 64] {
    sk.sign_raw(msg, &[0u8; 32]).unwrap().to_bytes()
}

/// A JSON string literal: the test content has no characters to escape.
fn js(s: &str) -> String {
    assert!(!s.contains(['"', '\\']) && !s.chars().any(char::is_control));
    format!("\"{s}\"")
}

/// A synthetic NIP-35 event, its id recomputed and signed by `signer`,
/// claiming `claimed` as its public key. Returns the relay message.
fn signed_msg(signer: &SigningKey, claimed: &[u8; 32], content: &str) -> String {
    let tags: Vec<Vec<String>> = [
        ["x", "0123456789abcdef0123456789abcdef01234567"].as_slice(),
        ["title", "Synthetic Test Release 2026"].as_slice(),
        ["t", "test"].as_slice(),
    ]
    .iter()
    .map(|t| t.iter().map(|s| s.to_string()).collect())
    .collect();
    let id = event_id(claimed, 1_700_000_000, KIND_TORRENT, &tags, content);
    let sig = sign(signer, &id);
    let tags_json: Vec<String> = tags
        .iter()
        .map(|t| {
            let items: Vec<String> = t.iter().map(|s| js(s)).collect();
            format!("[{}]", items.join(","))
        })
        .collect();
    format!(
        r#"["EVENT","sub1",{{"id":"{}","pubkey":"{}","created_at":1700000000,"kind":{KIND_TORRENT},"tags":[{}],"content":{},"sig":"{}"}}]"#,
        hex(&id),
        hex(claimed),
        tags_json.join(","),
        js(content),
        hex(&sig)
    )
}

fn own_key(sk: &SigningKey) -> [u8; 32] {
    sk.verifying_key().to_bytes().into()
}

#[test]
fn signed_event_is_kept() {
    let sk = test_key();
    let msg = signed_msg(&sk, &own_key(&sk), "A synthetic description.");
    let Ok(Message::Event { sub, event }) = parse_signed(msg.as_bytes(), &LIMITS) else {
        panic!("a correctly signed event is kept");
    };
    assert_eq!(sub, "sub1");
    assert!(event_signed(&event));
    let r = nostr::release(&event).expect("a NIP-35 release");
    assert_eq!(r.name.as_deref(), Some("Synthetic Test Release 2026"));
    assert_eq!(r.publisher, nostr::npub(&own_key(&sk)));
}

/// An event signed by one key but claiming another (a relay forging a
/// publisher): the id matches, the signature doesn't.
#[test]
fn forged_publisher_is_dropped() {
    let sk = test_key();
    let other = SigningKey::from_slice(&[0x42; 32]).unwrap();
    let msg = signed_msg(&other, &own_key(&sk), "forged");
    assert!(matches!(
        nostr::parse(msg.as_bytes(), &LIMITS),
        Ok(Message::Event { .. })
    ));
    assert_eq!(
        parse_signed(msg.as_bytes(), &LIMITS),
        Err(Rejected::BadSignature)
    );
}

/// The signature hex changed in one digit: dropped.
#[test]
fn tampered_signature_is_dropped() {
    let sk = test_key();
    let msg = signed_msg(&sk, &own_key(&sk), "tampered");
    let at = msg.find(r#""sig":""#).unwrap() + 7 + 70;
    let mut b = msg.into_bytes();
    b[at] = if b[at] == b'0' { b'1' } else { b'0' };
    assert_eq!(parse_signed(&b, &LIMITS), Err(Rejected::BadSignature));
}

/// The content edited after signing fails the id first, as NOS-0 refuses.
#[test]
fn edited_content_is_refused() {
    let sk = test_key();
    let msg = signed_msg(&sk, &own_key(&sk), "original").replace("original", "edited!!");
    assert_eq!(
        parse_signed(msg.as_bytes(), &LIMITS),
        Err(Rejected::Refused(Refused::IdMismatch))
    );
}

/// A key that isn't on the curve, with a well-formed id: a failed check,
/// not a panic.
#[test]
fn off_curve_publisher_is_dropped() {
    let sk = test_key();
    let bad = arr::<32>(&format!("{:064x}", 5));
    let msg = signed_msg(&sk, &bad, "off curve");
    assert_eq!(
        parse_signed(msg.as_bytes(), &LIMITS),
        Err(Rejected::BadSignature)
    );
}

/// Messages with no signature pass through as `nostr::parse` reads them.
#[test]
fn unsigned_messages_pass_through() {
    for m in [
        r#"["EOSE","sub1"]"#,
        r#"["NOTICE","hello"]"#,
        r#"["CLOSED","sub1","error: shutting down"]"#,
    ] {
        let want = nostr::parse(m.as_bytes(), &LIMITS).unwrap();
        assert_eq!(parse_signed(m.as_bytes(), &LIMITS), Ok(want), "{m}");
    }
    assert_eq!(
        parse_signed(br#"["AUTH","challenge"]"#, &LIMITS),
        Err(Rejected::Refused(Refused::Auth))
    );
    let refused = nostr::parse(b"not json", &LIMITS).unwrap_err();
    assert!(matches!(refused, Refused::Json(_)));
    assert_eq!(
        parse_signed(b"not json", &LIMITS),
        Err(Rejected::Refused(refused))
    );
}

/// Verification cost on this machine. Not a gate: run with
/// `cargo test --release -p lodestar-sources-feeds --test nostr_sig -- --ignored --nocapture`.
#[test]
#[ignore = "timing only; run by hand in release"]
fn verify_cost() {
    let (pk, msg, sig) = baseline();
    let n: u32 = 2_000;
    let t = std::time::Instant::now();
    let mut ok = 0;
    for _ in 0..n {
        ok += u32::from(verify(
            std::hint::black_box(&pk),
            std::hint::black_box(&msg),
            std::hint::black_box(&sig),
        ));
    }
    let per = t.elapsed() / n;
    assert_eq!(ok, n);
    println!("BIP-340 verify: {per:?} per signature");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Any bytes at all: a result, never a panic.
    #[test]
    fn arbitrary_bytes_never_panic(
        pk in any::<[u8; 32]>(),
        msg in proptest::collection::vec(any::<u8>(), 0..128),
        r in any::<[u8; 32]>(),
        s in any::<[u8; 32]>(),
    ) {
        let mut sig = [0u8; 64];
        sig[..32].copy_from_slice(&r);
        sig[32..].copy_from_slice(&s);
        let _ = verify(&pk, &msg, &sig);
        let _ = verify(&pk, &msg[..msg.len().min(32)], &sig);
    }

    /// Any text through `parse_signed`: a result, never a panic.
    #[test]
    fn arbitrary_messages_never_panic(s in "\\PC{0,256}") {
        let _ = parse_signed(s.as_bytes(), &LIMITS);
    }

    /// A valid signature of a 32-byte id fails once any single bit of the
    /// id, the signature or the public key is flipped.
    #[test]
    fn any_flipped_bit_fails(
        secret in any::<[u8; 32]>().prop_filter_map("a valid secret", |s| SigningKey::from_slice(&s).ok().map(|_| s)),
        id in any::<[u8; 32]>(),
        aux in any::<[u8; 32]>(),
        bit in 0usize..(32 + 64 + 32) * 8,
    ) {
        let sk = SigningKey::from_slice(&secret).unwrap();
        let pk = own_key(&sk);
        let sig = sk.sign_raw(&id, &aux).unwrap().to_bytes();
        prop_assert!(verify(&pk, &id, &sig));
        let (mut pk2, mut id2, mut sig2) = (pk, id, sig);
        let (byte, mask) = (bit / 8, 1u8 << (bit % 8));
        match byte {
            0..32 => id2[byte] ^= mask,
            32..96 => sig2[byte - 32] ^= mask,
            _ => pk2[byte - 96] ^= mask,
        }
        prop_assert!(!verify(&pk2, &id2, &sig2));
    }
}
