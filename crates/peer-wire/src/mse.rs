//! Message Stream Encryption / Protocol Encryption (MSE/PE), the
//! obfuscation handshake many peers require before they'll talk.
//!
//! Spec: the Vuze/Azureus MSE specification. In short: a Diffie-Hellman
//! exchange over a fixed 768-bit prime, then RC4 keyed by SHA-1 of the
//! shared secret and the info hash (the first 1024 keystream bytes are
//! dropped), with random padding and a sync search so the handshake has no
//! fixed byte pattern for traffic shapers to match.
//!
//! Both roles are here: the initiator is what the crawler uses; the
//! responder backs the test peer (and is checked against the initiator).

use num_bigint::BigUint;
use rand::RngExt;
use sha1::{Digest, Sha1};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::Error;
use crate::rc4::Rc4;

/// The MSE prime (768 bits), generator 2.
const P_HEX: &[u8] = b"FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74020BBEA63B139B22514A08798E3404DDEF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245E485B576625E7EC6F44C42E9A63A36210000000000090563";
const KEY_LEN: usize = 96;
const MAX_PAD: usize = 512;
const VC: [u8; 8] = [0; 8];

/// `crypto_provide` / `crypto_select` bits.
pub const PLAINTEXT: u32 = 0x01;
pub const RC4: u32 = 0x02;

/// The ciphers for the payload stream; `None` means plaintext.
#[derive(Debug)]
pub struct Ciphers {
    pub(crate) read: Option<Rc4>,
    pub(crate) write: Option<Rc4>,
}

fn prime() -> BigUint {
    BigUint::parse_bytes(P_HEX, 16).expect("valid prime")
}

fn hash(parts: &[&[u8]]) -> [u8; 20] {
    let mut h = Sha1::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

fn to_key_bytes(n: &BigUint) -> [u8; KEY_LEN] {
    let bytes = n.to_bytes_be();
    let mut out = [0u8; KEY_LEN];
    out[KEY_LEN - bytes.len()..].copy_from_slice(&bytes);
    out
}

struct KeyPair {
    private: BigUint,
    public: [u8; KEY_LEN],
}

fn keypair(rng: &mut impl RngExt) -> KeyPair {
    // 160 random bits, as the spec recommends.
    let x: [u8; 20] = rng.random();
    let private = BigUint::from_bytes_be(&x);
    let public = to_key_bytes(&BigUint::from(2u32).modpow(&private, &prime()));
    KeyPair { private, public }
}

/// The shared secret, rejecting degenerate public keys (0, 1, P-1 and
/// anything ≥ P), which would make the secret predictable.
fn shared_secret(theirs: &[u8; KEY_LEN], ours: &KeyPair) -> Result<[u8; KEY_LEN], Error> {
    let p = prime();
    let y = BigUint::from_bytes_be(theirs);
    if y <= BigUint::from(1u32) || y >= &p - 1u32 {
        return Err(Error::Mse("degenerate public key"));
    }
    Ok(to_key_bytes(&y.modpow(&ours.private, &p)))
}

fn pad(rng: &mut impl RngExt) -> Vec<u8> {
    let len = rng.random_range(0..=MAX_PAD);
    (0..len).map(|_| rng.random()).collect()
}

/// Reads byte by byte until the last `pattern.len()` bytes equal `pattern`,
/// giving up after `limit` bytes.
async fn sync_to<S: AsyncRead + Unpin>(
    stream: &mut S,
    pattern: &[u8],
    limit: usize,
) -> Result<(), Error> {
    let mut window = Vec::with_capacity(pattern.len());
    for _ in 0..limit {
        let b = stream.read_u8().await?;
        if window.len() == pattern.len() {
            window.remove(0);
        }
        window.push(b);
        if window == pattern {
            return Ok(());
        }
    }
    Err(Error::Mse("sync marker not found"))
}

fn select(offered: u32, allowed: u32) -> Option<u32> {
    let both = offered & allowed;
    // Prefer RC4 when both sides allow it.
    if both & RC4 != 0 {
        Some(RC4)
    } else if both & PLAINTEXT != 0 {
        Some(PLAINTEXT)
    } else {
        None
    }
}

/// Runs the initiator side. `provide` is the set of methods we accept;
/// `initial` (IA) is sent encrypted inside the handshake, which saves a
/// round trip for the BitTorrent handshake that follows.
pub async fn initiate<S>(
    stream: &mut S,
    info_hash: &[u8; 20],
    provide: u32,
    initial: &[u8],
    rng: &mut impl RngExt,
) -> Result<(Ciphers, u32), Error>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let keys = keypair(rng);
    let mut first = keys.public.to_vec();
    first.extend(pad(rng));
    stream.write_all(&first).await?;

    let mut their_public = [0u8; KEY_LEN];
    stream.read_exact(&mut their_public).await?;
    let secret = shared_secret(&their_public, &keys)?;

    let mut encrypt = Rc4::mse(&hash(&[b"keyA", &secret, info_hash]));
    let mut decrypt = Rc4::mse(&hash(&[b"keyB", &secret, info_hash]));

    // Step 3: HASH('req1', S), HASH('req2', SKEY) ^ HASH('req3', S), then the
    // encrypted VC, crypto_provide, padding and initial payload.
    let ia_len =
        u16::try_from(initial.len()).map_err(|_| Error::Mse("initial payload too long"))?;
    let req2 = hash(&[b"req2", info_hash]);
    let req3 = hash(&[b"req3", &secret]);
    let mut msg = hash(&[b"req1", &secret]).to_vec();
    msg.extend(req2.iter().zip(req3.iter()).map(|(a, b)| a ^ b));
    let mut encrypted = VC.to_vec();
    encrypted.extend_from_slice(&provide.to_be_bytes());
    encrypted.extend_from_slice(&0u16.to_be_bytes()); // no PadC
    encrypted.extend_from_slice(&ia_len.to_be_bytes());
    encrypted.extend_from_slice(initial);
    encrypt.apply(&mut encrypted);
    msg.extend(encrypted);
    stream.write_all(&msg).await?;

    // Step 4: find the encrypted VC after the responder's padding.
    let mut marker = VC;
    decrypt.clone().apply(&mut marker);
    sync_to(stream, &marker, MAX_PAD + VC.len()).await?;
    decrypt.apply(&mut [0u8; 8]);
    let mut head = [0u8; 6];
    stream.read_exact(&mut head).await?;
    decrypt.apply(&mut head);
    let selected = u32::from_be_bytes(head[..4].try_into().expect("4 bytes"));
    let pad_len = usize::from(u16::from_be_bytes([head[4], head[5]]));
    if pad_len > MAX_PAD {
        return Err(Error::Mse("padding too long"));
    }
    let mut pad_d = vec![0u8; pad_len];
    stream.read_exact(&mut pad_d).await?;
    decrypt.apply(&mut pad_d);
    if selected.count_ones() != 1 || selected & provide == 0 {
        return Err(Error::Mse("peer selected a method we didn't offer"));
    }
    let ciphers = if selected == RC4 {
        Ciphers {
            read: Some(decrypt),
            write: Some(encrypt),
        }
    } else {
        Ciphers {
            read: None,
            write: None,
        }
    };
    Ok((ciphers, selected))
}

/// The responder side, for peers we serve (the test peer). Identifies the
/// torrent among `known` and returns its index, the payload ciphers and
/// the initiator's initial payload.
pub async fn respond<S>(
    stream: &mut S,
    known: &[[u8; 20]],
    allowed: u32,
    rng: &mut impl RngExt,
) -> Result<(usize, Ciphers, Vec<u8>), Error>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut their_public = [0u8; KEY_LEN];
    stream.read_exact(&mut their_public).await?;
    let keys = keypair(rng);
    let mut reply = keys.public.to_vec();
    reply.extend(pad(rng));
    stream.write_all(&reply).await?;
    let secret = shared_secret(&their_public, &keys)?;

    sync_to(stream, &hash(&[b"req1", &secret]), MAX_PAD + 20).await?;
    let mut obfuscated = [0u8; 20];
    stream.read_exact(&mut obfuscated).await?;
    let req3 = hash(&[b"req3", &secret]);
    let which = known
        .iter()
        .position(|h| {
            let req2 = hash(&[b"req2", h]);
            req2.iter()
                .zip(req3.iter())
                .map(|(a, b)| a ^ b)
                .eq(obfuscated.iter().copied())
        })
        .ok_or(Error::Mse("unknown info hash"))?;
    let info_hash = &known[which];

    let mut decrypt = Rc4::mse(&hash(&[b"keyA", &secret, info_hash]));
    let mut encrypt = Rc4::mse(&hash(&[b"keyB", &secret, info_hash]));
    let mut head = [0u8; 14];
    stream.read_exact(&mut head).await?;
    decrypt.apply(&mut head);
    if head[..8] != VC {
        return Err(Error::Mse("bad verification constant"));
    }
    let offered = u32::from_be_bytes(head[8..12].try_into().expect("4 bytes"));
    let pad_len = usize::from(u16::from_be_bytes([head[12], head[13]]));
    if pad_len > MAX_PAD {
        return Err(Error::Mse("padding too long"));
    }
    let mut pad_c = vec![0u8; pad_len + 2];
    stream.read_exact(&mut pad_c).await?;
    decrypt.apply(&mut pad_c);
    let ia_len = usize::from(u16::from_be_bytes([pad_c[pad_len], pad_c[pad_len + 1]]));
    let mut initial = vec![0u8; ia_len];
    stream.read_exact(&mut initial).await?;
    decrypt.apply(&mut initial);

    let selected = select(offered, allowed).ok_or(Error::Mse("no common method"))?;
    let mut answer = VC.to_vec();
    answer.extend_from_slice(&selected.to_be_bytes());
    let pad_d = pad(rng);
    answer.extend_from_slice(&(pad_d.len() as u16).to_be_bytes());
    answer.extend(pad_d);
    encrypt.apply(&mut answer);
    stream.write_all(&answer).await?;
    let ciphers = if selected == RC4 {
        Ciphers {
            read: Some(decrypt),
            write: Some(encrypt),
        }
    } else {
        Ciphers {
            read: None,
            write: None,
        }
    };
    Ok((which, ciphers, initial))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    #[test]
    fn dh_agrees_and_rejects_degenerate_keys() {
        let mut rng = rand::rngs::StdRng::seed_from_u64(1);
        let a = keypair(&mut rng);
        let b = keypair(&mut rng);
        assert_eq!(
            shared_secret(&b.public, &a).unwrap(),
            shared_secret(&a.public, &b).unwrap()
        );
        assert!(shared_secret(&[0; KEY_LEN], &a).is_err());
        let mut one = [0u8; KEY_LEN];
        one[KEY_LEN - 1] = 1;
        assert!(shared_secret(&one, &a).is_err());
        let p_minus_1 = to_key_bytes(&(prime() - 1u32));
        assert!(shared_secret(&p_minus_1, &a).is_err());
        assert!(shared_secret(&[0xff; KEY_LEN], &a).is_err(), "≥ P");
    }

    #[test]
    fn method_selection_prefers_rc4() {
        assert_eq!(select(RC4 | PLAINTEXT, RC4 | PLAINTEXT), Some(RC4));
        assert_eq!(select(RC4 | PLAINTEXT, PLAINTEXT), Some(PLAINTEXT));
        assert_eq!(select(PLAINTEXT, RC4), None);
    }

    async fn handshake(provide: u32, allowed: u32) -> Result<(u32, Vec<u8>), Error> {
        let (mut a, mut b) = tokio::io::duplex(4096);
        let hash = [7u8; 20];
        let responder = tokio::spawn(async move {
            let mut rng = rand::rngs::StdRng::seed_from_u64(2);
            let (idx, mut ciphers, ia) =
                respond(&mut b, &[[1; 20], hash], allowed, &mut rng).await?;
            assert_eq!(idx, 1);
            // Echo a payload back through the negotiated method.
            let mut msg = b"pong".to_vec();
            if let Some(c) = ciphers.write.as_mut() {
                c.apply(&mut msg);
            }
            b.write_all(&msg).await?;
            Ok::<_, Error>(ia)
        });
        let mut rng = rand::rngs::StdRng::seed_from_u64(3);
        let (mut ciphers, selected) = initiate(&mut a, &hash, provide, b"hello", &mut rng).await?;
        let mut buf = [0u8; 4];
        a.read_exact(&mut buf).await?;
        if let Some(c) = ciphers.read.as_mut() {
            c.apply(&mut buf);
        }
        assert_eq!(&buf, b"pong");
        let ia = responder.await.unwrap()?;
        Ok((selected, ia))
    }

    #[tokio::test]
    async fn initiator_and_responder_agree() {
        assert_eq!(
            handshake(RC4 | PLAINTEXT, RC4 | PLAINTEXT).await.unwrap(),
            (RC4, b"hello".to_vec())
        );
        assert_eq!(
            handshake(RC4 | PLAINTEXT, PLAINTEXT).await.unwrap(),
            (PLAINTEXT, b"hello".to_vec())
        );
        assert_eq!(handshake(RC4, RC4).await.unwrap(), (RC4, b"hello".to_vec()));
        assert!(handshake(PLAINTEXT, RC4).await.is_err());
    }
}
