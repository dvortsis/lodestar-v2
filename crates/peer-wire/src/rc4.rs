//! RC4, as MSE/PE requires (with the first 1024 keystream bytes dropped).
//! RC4 is broken as a cipher; MSE uses it for protocol obfuscation, which
//! is all peers need it for.

#[derive(Clone)]
pub(crate) struct Rc4 {
    s: [u8; 256],
    i: u8,
    j: u8,
}

impl Rc4 {
    pub fn new(key: &[u8]) -> Rc4 {
        assert!(!key.is_empty() && key.len() <= 256, "RC4 key length");
        let mut s = [0u8; 256];
        for (i, b) in s.iter_mut().enumerate() {
            *b = i as u8;
        }
        let mut j = 0u8;
        for i in 0..256 {
            j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
            s.swap(i, usize::from(j));
        }
        Rc4 { s, i: 0, j: 0 }
    }

    /// The MSE flavour: RC4 with the first 1024 bytes of keystream dropped.
    pub fn mse(key: &[u8]) -> Rc4 {
        let mut rc4 = Rc4::new(key);
        rc4.apply(&mut [0u8; 1024]);
        rc4
    }

    /// XORs the keystream into `data` (encrypts and decrypts alike).
    pub fn apply(&mut self, data: &mut [u8]) {
        for b in data {
            self.i = self.i.wrapping_add(1);
            self.j = self.j.wrapping_add(self.s[usize::from(self.i)]);
            self.s.swap(usize::from(self.i), usize::from(self.j));
            let k = self.s[usize::from(
                self.s[usize::from(self.i)].wrapping_add(self.s[usize::from(self.j)]),
            )];
            *b ^= k;
        }
    }
}

impl std::fmt::Debug for Rc4 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Rc4(..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 6229 test vectors (40-bit and 128-bit keys, offset 0).
    #[test]
    fn rfc6229_vectors() {
        let mut out = [0u8; 16];
        Rc4::new(&[0x01, 0x02, 0x03, 0x04, 0x05]).apply(&mut out);
        assert_eq!(
            out,
            [
                0xb2, 0x39, 0x63, 0x05, 0xf0, 0x3d, 0xc0, 0x27, 0xcc, 0xc3, 0x52, 0x4a, 0x0a, 0x11,
                0x18, 0xa8
            ]
        );
        let key: Vec<u8> = (1..=16).collect();
        let mut out = [0u8; 16];
        Rc4::new(&key).apply(&mut out);
        assert_eq!(
            out,
            [
                0x9a, 0xc7, 0xcc, 0x9a, 0x60, 0x9d, 0x1e, 0xf7, 0xb2, 0x93, 0x28, 0x99, 0xcd, 0xe4,
                0x1b, 0x97
            ]
        );
    }

    /// 128-bit key, keystream at offset 1024 (what MSE uses first); value
    /// cross-checked against an independent implementation.
    #[test]
    fn rfc6229_offset_1024() {
        let key: Vec<u8> = (1..=16).collect();
        let mut out = [0u8; 16];
        Rc4::mse(&key).apply(&mut out);
        assert_eq!(
            out,
            [
                0xbd, 0xf0, 0x32, 0x4e, 0x60, 0x83, 0xdc, 0xc6, 0xd3, 0xce, 0xdd, 0x3c, 0xa8, 0xc5,
                0x3c, 0x16
            ]
        );
    }

    #[test]
    fn roundtrip() {
        let mut data = b"attack at dawn".to_vec();
        Rc4::mse(b"k").apply(&mut data);
        assert_ne!(&data, b"attack at dawn");
        Rc4::mse(b"k").apply(&mut data);
        assert_eq!(&data, b"attack at dawn");
    }
}
