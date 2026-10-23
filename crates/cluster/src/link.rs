//! One side of a coordinator–worker connection, over either transport
//! (§19.2): a WebSocket over mutual TLS for remote workers, or in-process
//! channels for the coordinator's built-in local worker. Sessions are
//! written once against [`Link`], so a single machine and a fleet run the
//! same code path.

use std::marker::PhantomData;

use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use crate::frame::{self, FrameError, MAX_FRAME};
use crate::proto::{ToCoordinator, ToWorker};

/// Any byte stream a WebSocket can run over (a TLS stream, in practice).
pub(crate) trait Io: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Io for T {}
pub(crate) type BoxIo = Box<dyn Io>;

/// The WebSocket limits on both sides: one frame per message, nothing
/// larger than a cluster frame, small buffers.
pub(crate) fn ws_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .read_buffer_size(16 * 1024)
        .write_buffer_size(0)
        .max_write_buffer_size(4 * MAX_FRAME)
        .max_message_size(Some(MAX_FRAME))
        .max_frame_size(Some(MAX_FRAME))
}

#[derive(Debug)]
pub enum LinkError {
    /// The other side is gone.
    Closed,
    Io(std::io::ErrorKind),
    Frame(FrameError),
    /// A text or otherwise unexpected WebSocket message.
    Protocol,
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkError::Closed => f.write_str("the connection closed"),
            LinkError::Io(kind) => write!(f, "connection error: {kind}"),
            LinkError::Frame(e) => write!(f, "bad frame: {e}"),
            LinkError::Protocol => f.write_str("unexpected WebSocket message"),
        }
    }
}

impl std::error::Error for LinkError {}

fn ws_error(e: tokio_tungstenite::tungstenite::Error) -> LinkError {
    use tokio_tungstenite::tungstenite::Error as E;
    match e {
        E::ConnectionClosed | E::AlreadyClosed => LinkError::Closed,
        E::Io(e) => LinkError::Io(e.kind()),
        E::Capacity(_) => LinkError::Frame(FrameError::TooLarge),
        _ => LinkError::Protocol,
    }
}

/// How a message type is read off the wire, with its limits.
pub trait Inbound: Sized + Send + 'static {
    fn decode(frame: &[u8]) -> Result<Self, FrameError>;
    fn validate(&self) -> bool;
}

impl Inbound for ToCoordinator {
    fn decode(frame: &[u8]) -> Result<Self, FrameError> {
        frame::decode_to_coordinator(frame)
    }
    fn validate(&self) -> bool {
        ToCoordinator::validate(self)
    }
}

impl Inbound for ToWorker {
    fn decode(frame: &[u8]) -> Result<Self, FrameError> {
        frame::decode_to_worker(frame)
    }
    fn validate(&self) -> bool {
        ToWorker::validate(self)
    }
}

enum Transport<Tx, Rx> {
    Ws(Box<WebSocketStream<BoxIo>>),
    Local {
        tx: mpsc::Sender<Tx>,
        rx: mpsc::Receiver<Rx>,
    },
}

/// Sends `Tx` and receives `Rx`.
pub struct Link<Tx, Rx> {
    transport: Transport<Tx, Rx>,
    _types: PhantomData<fn(Tx) -> Rx>,
}

/// The coordinator's end.
pub type CoordinatorLink = Link<ToWorker, ToCoordinator>;
/// A worker's end.
pub type WorkerLink = Link<ToCoordinator, ToWorker>;

impl<Tx, Rx> std::fmt::Debug for Link<Tx, Rx> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self.transport {
            Transport::Ws(_) => "Link(WebSocket)",
            Transport::Local { .. } => "Link(local)",
        })
    }
}

/// A connected pair of in-process links, each direction holding at most
/// `capacity` messages.
pub fn local_pair(capacity: usize) -> (CoordinatorLink, WorkerLink) {
    let (to_worker, from_coordinator) = mpsc::channel(capacity);
    let (to_coordinator, from_worker) = mpsc::channel(capacity);
    (
        Link::local(to_worker, from_worker),
        Link::local(to_coordinator, from_coordinator),
    )
}

impl<Tx: Serialize + Send + 'static, Rx: Inbound> Link<Tx, Rx> {
    fn local(tx: mpsc::Sender<Tx>, rx: mpsc::Receiver<Rx>) -> Self {
        Link {
            transport: Transport::Local { tx, rx },
            _types: PhantomData,
        }
    }

    pub(crate) fn ws(ws: WebSocketStream<BoxIo>) -> Self {
        Link {
            transport: Transport::Ws(Box::new(ws)),
            _types: PhantomData,
        }
    }

    pub fn is_local(&self) -> bool {
        matches!(self.transport, Transport::Local { .. })
    }

    /// Sends one message. Over the WebSocket, a message too large for a
    /// frame is refused here (a bug on this side). In process, messages
    /// aren't encoded; the receiver checks the same limits (`validate`),
    /// and every message within them fits in a frame (tested in `frame`).
    pub async fn send(&mut self, msg: Tx) -> Result<(), LinkError> {
        match &mut self.transport {
            Transport::Ws(ws) => {
                let bytes = frame::encode(&msg).map_err(LinkError::Frame)?;
                ws.send(Message::Binary(bytes.into()))
                    .await
                    .map_err(ws_error)
            }
            Transport::Local { tx, .. } => tx.send(msg).await.map_err(|_| LinkError::Closed),
        }
    }

    /// The next message, or `None` once the other side has closed. It is
    /// cancel-safe: a message is never lost by dropping the future.
    pub async fn recv(&mut self) -> Result<Option<Rx>, LinkError> {
        match &mut self.transport {
            Transport::Ws(ws) => loop {
                match ws.next().await {
                    None => return Ok(None),
                    Some(Err(e)) => {
                        return match ws_error(e) {
                            LinkError::Closed => Ok(None),
                            e => Err(e),
                        };
                    }
                    Some(Ok(Message::Binary(bytes))) => {
                        return Rx::decode(&bytes).map(Some).map_err(LinkError::Frame);
                    }
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                    Some(Ok(Message::Close(_))) => return Ok(None),
                    Some(Ok(Message::Text(_) | Message::Frame(_))) => {
                        return Err(LinkError::Protocol);
                    }
                }
            },
            Transport::Local { rx, .. } => match rx.recv().await {
                None => Ok(None),
                Some(msg) if msg.validate() => Ok(Some(msg)),
                Some(_) => Err(LinkError::Frame(FrameError::Invalid)),
            },
        }
    }

    /// Closes the connection (best effort).
    pub async fn close(&mut self) {
        match &mut self.transport {
            Transport::Ws(ws) => {
                let _ = WebSocketStream::close(ws, None).await;
            }
            Transport::Local { rx, .. } => rx.close(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::*;

    #[tokio::test]
    async fn local_links_carry_messages_both_ways_and_check_limits() {
        let (mut coord, mut worker) = local_pair(4);
        assert!(coord.is_local());
        worker.send(ToCoordinator::Drained).await.unwrap();
        assert_eq!(coord.recv().await.unwrap(), Some(ToCoordinator::Drained));
        coord.send(ToWorker::Drain).await.unwrap();
        assert_eq!(worker.recv().await.unwrap(), Some(ToWorker::Drain));

        // Over a limit: refused on receipt, as the wire would.
        let bad = ToCoordinator::Claim(ClaimBatch {
            batch: 1,
            items: vec![],
        });
        worker.send(bad).await.unwrap();
        assert!(matches!(
            coord.recv().await,
            Err(LinkError::Frame(FrameError::Invalid))
        ));
        coord
            .send(ToWorker::HaveSnapshot(vec![0; MAX_OPAQUE + 1]))
            .await
            .unwrap();
        assert!(matches!(
            worker.recv().await,
            Err(LinkError::Frame(FrameError::Invalid))
        ));

        drop(worker);
        assert_eq!(coord.recv().await.unwrap(), None);
        assert!(matches!(
            coord.send(ToWorker::Drain).await,
            Err(LinkError::Closed)
        ));
    }

    #[tokio::test]
    async fn websocket_links_carry_frames_and_refuse_text() {
        let (a, b) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(async move {
            let ws = tokio_tungstenite::accept_async_with_config(
                Box::new(a) as BoxIo,
                Some(ws_config()),
            )
            .await
            .unwrap();
            let mut link: CoordinatorLink = Link::ws(ws);
            let first = link.recv().await.unwrap();
            link.send(ToWorker::Drain).await.unwrap();
            let second = link.recv().await;
            (first, second.map_err(|e| e.to_string()))
        });
        let (ws, _) = tokio_tungstenite::client_async_with_config(
            "wss://coordinator.cluster.lodestar.invalid/cluster",
            Box::new(b) as BoxIo,
            Some(ws_config()),
        )
        .await
        .unwrap();
        let mut ws = ws;
        let hello = ToCoordinator::Heartbeat(Heartbeat::default());
        ws.send(Message::Binary(frame::encode(&hello).unwrap().into()))
            .await
            .unwrap();
        let reply = ws.next().await.unwrap().unwrap();
        assert_eq!(
            frame::decode_to_worker(&reply.into_data()).unwrap(),
            ToWorker::Drain
        );
        ws.send(Message::Text("hi".into())).await.unwrap();
        let (first, second) = server.await.unwrap();
        assert_eq!(first, Some(hello));
        assert!(second.is_err());
    }
}
