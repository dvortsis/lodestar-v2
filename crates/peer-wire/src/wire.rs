//! Length-prefixed peer-wire messages over a stream, with optional RC4 in
//! each direction (after MSE).

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::Error;
use crate::mse::Ciphers;

/// Message id of BEP 10 extended messages.
pub(crate) const EXTENDED: u8 = 20;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Incoming {
    KeepAlive,
    /// An extended message: `[extended id, payload…]`.
    Extended(Vec<u8>),
    /// Any other message, already skipped.
    Other(u8),
}

pub(crate) struct Wire<S> {
    stream: S,
    ciphers: Ciphers,
    /// Payload bytes that arrived inside a handshake (MSE's IA), already
    /// decrypted, to be read before the stream.
    prefix: Vec<u8>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Wire<S> {
    pub fn new(stream: S, ciphers: Ciphers, prefix: Vec<u8>) -> Wire<S> {
        Wire {
            stream,
            ciphers,
            prefix,
        }
    }

    pub async fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
        let from_prefix = buf.len().min(self.prefix.len());
        buf[..from_prefix].copy_from_slice(&self.prefix[..from_prefix]);
        self.prefix.drain(..from_prefix);
        let rest = &mut buf[from_prefix..];
        if !rest.is_empty() {
            self.stream.read_exact(rest).await?;
            if let Some(c) = self.ciphers.read.as_mut() {
                c.apply(rest);
            }
        }
        Ok(())
    }

    pub async fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        match self.ciphers.write.as_mut() {
            Some(c) => {
                let mut copy = data.to_vec();
                c.apply(&mut copy);
                self.stream.write_all(&copy).await?;
            }
            None => self.stream.write_all(data).await?,
        }
        self.stream.flush().await
    }

    /// Reads one message. Extended messages up to `max_extended` bytes are
    /// returned; other messages up to `max_other` bytes are read and
    /// discarded (still decrypted, to keep the cipher in step).
    pub async fn read_message(
        &mut self,
        max_extended: usize,
        max_other: usize,
    ) -> Result<Incoming, Error> {
        let mut len = [0u8; 4];
        self.read_exact(&mut len).await?;
        let len = u32::from_be_bytes(len) as usize;
        if len == 0 {
            return Ok(Incoming::KeepAlive);
        }
        let mut id = [0u8; 1];
        self.read_exact(&mut id).await?;
        let body = len - 1;
        if id[0] == EXTENDED {
            if body == 0 || body > max_extended {
                return Err(Error::Protocol("extended message size"));
            }
            let mut payload = vec![0u8; body];
            self.read_exact(&mut payload).await?;
            return Ok(Incoming::Extended(payload));
        }
        if body > max_other {
            return Err(Error::Protocol("message too large"));
        }
        let mut scratch = [0u8; 4096];
        let mut left = body;
        while left > 0 {
            let n = left.min(scratch.len());
            self.read_exact(&mut scratch[..n]).await?;
            left -= n;
        }
        Ok(Incoming::Other(id[0]))
    }

    pub async fn send_extended(&mut self, ext_id: u8, payload: &[u8]) -> io::Result<()> {
        let mut msg = Vec::with_capacity(payload.len() + 6);
        msg.extend_from_slice(&(payload.len() as u32 + 2).to_be_bytes());
        msg.push(EXTENDED);
        msg.push(ext_id);
        msg.extend_from_slice(payload);
        self.write_all(&msg).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain() -> Ciphers {
        Ciphers {
            read: None,
            write: None,
        }
    }

    #[tokio::test]
    async fn frames_and_skips() {
        let (a, b) = tokio::io::duplex(1 << 16);
        let mut w = Wire::new(a, plain(), Vec::new());
        let mut r = Wire::new(b, plain(), Vec::new());
        w.write_all(&[0, 0, 0, 0]).await.unwrap(); // keep-alive
        w.write_all(&[0, 0, 0, 3, 5, 0xaa, 0xbb]).await.unwrap(); // bitfield
        w.send_extended(3, b"hi").await.unwrap();
        assert_eq!(r.read_message(100, 100).await.unwrap(), Incoming::KeepAlive);
        assert_eq!(r.read_message(100, 100).await.unwrap(), Incoming::Other(5));
        assert_eq!(
            r.read_message(100, 100).await.unwrap(),
            Incoming::Extended(vec![3, b'h', b'i'])
        );
    }

    #[tokio::test]
    async fn rejects_oversize_messages() {
        let (a, b) = tokio::io::duplex(1 << 16);
        let mut w = Wire::new(a, plain(), Vec::new());
        let mut r = Wire::new(b, plain(), Vec::new());
        w.send_extended(1, &[0; 50]).await.unwrap();
        assert!(matches!(
            r.read_message(10, 10).await,
            Err(Error::Protocol(_))
        ));
    }

    #[tokio::test]
    async fn prefix_bytes_come_first() {
        let (_a, b) = tokio::io::duplex(64);
        let mut r = Wire::new(b, plain(), b"abc".to_vec());
        let mut buf = [0u8; 2];
        r.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ab");
        let mut buf = [0u8; 1];
        r.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"c");
    }
}
