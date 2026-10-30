//! The fixture replay server every adapter test uses.
//!
//! A fixture is one exchange recorded from the real client
//! (`tests/fixtures/<client>/<name>.json`): the request's method and path,
//! and the response's status, headers and body. A test scripts the
//! exchanges it expects, in order; the server checks each request against
//! the next entry and answers with its recorded response. Anything
//! unexpected gets a 599 and fails [`ReplayServer::finish`].

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::body::{Body, to_bytes};
use axum::extract::Request;
use axum::response::Response;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Fixture {
    pub request: FixtureRequest,
    pub response: FixtureResponse,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FixtureRequest {
    pub method: String,
    pub path: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FixtureResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

fn fixtures_dir(client: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(client)
}

/// Loads `tests/fixtures/<client>/<name>.json`.
pub fn fixture(client: &str, name: &str) -> Fixture {
    let path = fixtures_dir(client).join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path:?}: {e}"))
}

/// Loads a binary file from the client's fixture directory.
pub fn fixture_bytes(client: &str, name: &str) -> Vec<u8> {
    std::fs::read(fixtures_dir(client).join(name)).unwrap()
}

/// A request as the server received it.
#[derive(Debug, Clone)]
pub struct Received {
    pub method: String,
    pub path: String,
    pub query: Option<String>,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Received {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The value of a multipart text field (or `None`).
    pub fn multipart_field(&self, name: &str) -> Option<String> {
        let body = self.body_text();
        let marker = format!("name=\"{name}\"");
        let start = body.find(&marker)? + marker.len();
        let rest = &body[start..];
        let value_start = rest.find("\r\n\r\n")? + 4;
        let value = &rest[value_start..];
        Some(value[..value.find("\r\n--")?].to_owned())
    }

    /// The value of a urlencoded form field (or `None`).
    pub fn form_field(&self, name: &str) -> Option<String> {
        self.body_text().split('&').find_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            (k == name).then(|| decode(v))
        })
    }
}

fn decode(s: &str) -> String {
    let s = s.replace('+', " ");
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 3 <= bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Holds a [`refused_addr`] port: the two ends of a connection to it.
pub struct Held(std::net::TcpStream, std::net::TcpStream);

/// A loopback address where connecting is refused, for as long as the
/// returned [`Held`] lives.
///
/// The port's listener is closed, so a new connection is refused at
/// once on every OS. A connection accepted on it stays open, so the OS
/// counts the port as in use and never gives it to another socket bound
/// to port 0, which is how every [`ReplayServer`] binds. (Dropping the
/// listener alone freed the port: a server another test started could be
/// given it, answer, and lose a scripted reply its own test needed. A
/// socket that is bound but doesn't listen holds the port too, but macOS
/// leaves a connection to it unanswered until the client times out.)
pub fn refused_addr() -> (Held, std::net::SocketAddr) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let client = std::net::TcpStream::connect(addr).unwrap();
    let (server, _) = listener.accept().unwrap();
    (Held(client, server), addr)
}

#[derive(Default)]
struct State {
    script: Vec<Fixture>,
    received: Vec<Received>,
    unexpected: Vec<String>,
}

pub struct ReplayServer {
    port: u16,
    state: Arc<Mutex<State>>,
}

impl ReplayServer {
    /// Serves the scripted fixtures, in order, on a loopback port.
    pub async fn start(script: Vec<Fixture>) -> ReplayServer {
        let state = Arc::new(Mutex::new(State {
            script,
            ..State::default()
        }));
        let shared = Arc::clone(&state);
        let app = axum::Router::new().fallback(move |req: Request| {
            let state = Arc::clone(&shared);
            async move { answer(state, req).await }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        ReplayServer { port, state }
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }

    pub fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn received(&self) -> Vec<Received> {
        self.state.lock().unwrap().received.clone()
    }

    /// Asserts every scripted exchange happened and nothing else did, and
    /// returns the requests.
    pub fn finish(&self) -> Vec<Received> {
        let state = self.state.lock().unwrap();
        assert!(
            state.unexpected.is_empty(),
            "unexpected requests: {:?}",
            state.unexpected
        );
        let left: Vec<_> = state
            .script
            .iter()
            .map(|f| format!("{} {}", f.request.method, f.request.path))
            .collect();
        assert!(left.is_empty(), "scripted requests never made: {left:?}");
        state.received.clone()
    }
}

async fn answer(state: Arc<Mutex<State>>, req: Request) -> Response {
    let method = req.method().to_string();
    let path = req.uri().path().to_owned();
    let query = req.uri().query().map(str::to_owned);
    let headers = req
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("<binary>").to_owned()))
        .collect();
    let body = to_bytes(req.into_body(), 64 << 20).await.unwrap().to_vec();
    let mut state = state.lock().unwrap();
    state.received.push(Received {
        method: method.clone(),
        path: path.clone(),
        query,
        headers,
        body,
    });
    let next = match state.script.first() {
        Some(f) if f.request.method == method && f.request.path == path => {
            Some(state.script.remove(0))
        }
        _ => None,
    };
    let Some(fixture) = next else {
        state.unexpected.push(format!("{method} {path}"));
        return Response::builder()
            .status(599)
            .body(Body::from("unexpected request"))
            .unwrap();
    };
    let mut resp = Response::builder().status(fixture.response.status);
    for (k, v) in &fixture.response.headers {
        // The server sets its own framing.
        if !k.eq_ignore_ascii_case("content-length") && !k.eq_ignore_ascii_case("connection") {
            resp = resp.header(k, v);
        }
    }
    resp.body(Body::from(fixture.response.body)).unwrap()
}
