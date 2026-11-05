//! A streamed body keeps memory flat (SRC-12b): 512 MiB through the kit's
//! [`Client::poll_stream`], with the process's resident memory checked
//! after every chunk on Linux (`/proc/self/status`). Elsewhere the
//! unit test `a_large_stream_is_never_read_ahead` (in `http.rs`) still
//! shows the kit reads nothing ahead of the caller.
//!
//! Alone in its own test binary, so no other test's allocations move the
//! number.

use std::sync::Arc;
use std::time::Duration;

use lodestar_sources::http::{BodyStream, StreamLimits, StreamResponse};
use lodestar_sources::{
    Backoff, BoxFuture, Client, Fetch, HostLimiter, HostRate, HttpError, Limits, Request, Response,
};

const CHUNK: usize = 64 * 1024;
/// Twice the full scrape's cap, under the kit's ceiling.
const TOTAL: u64 = 512 * 1024 * 1024;
/// Resident growth allowed while streaming: room for the allocator and
/// the runtime, far below the body.
const RSS_BOUND: u64 = 16 * 1024 * 1024;

struct Endless;

struct EndlessBody {
    left: u64,
}

impl BodyStream for EndlessBody {
    fn next_chunk(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, HttpError>> {
        let chunk = (self.left > 0).then(|| {
            let n = self.left.min(CHUNK as u64);
            self.left -= n;
            // Written to, so its pages are really resident.
            vec![0xa5; n as usize]
        });
        Box::pin(async move { Ok(chunk) })
    }
}

impl Fetch for Endless {
    fn fetch<'a>(&'a self, _: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>> {
        Box::pin(async { Err(HttpError::Transport("streams only".into())) })
    }

    fn fetch_stream<'a>(
        &'a self,
        _: &'a Request,
        _: StreamLimits,
    ) -> BoxFuture<'a, Result<StreamResponse, HttpError>> {
        Box::pin(async {
            Ok(StreamResponse {
                status: 200,
                headers: Vec::new(),
                body: Box::new(EndlessBody { left: TOTAL }),
            })
        })
    }
}

/// Resident bytes, where the platform tells us.
fn resident() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

#[tokio::test(start_paused = true)]
async fn a_large_streamed_body_keeps_memory_flat() {
    let limits = Limits {
        poll_interval: Duration::from_secs(60),
        per_host: HostRate {
            interval: Duration::from_secs(1),
            burst: 1,
        },
        max_full_scrapes_per_day: 1,
        backoff: Backoff {
            base: Duration::from_secs(1),
            max: Duration::from_secs(60),
        },
    };
    let fetch: Arc<dyn Fetch> = Arc::new(Endless);
    let client = Client::new(fetch, HostLimiter::with_seed(limits, 1));
    let mut res = client
        .poll_stream(
            "t",
            &Request::get("http://t.test/scrape"),
            StreamLimits {
                max_bytes: TOTAL,
                timeout: Duration::from_secs(600),
                idle: Duration::from_secs(60),
            },
        )
        .await
        .unwrap();
    // The first chunk warms the allocator before the baseline.
    let mut read = res.next_chunk().await.unwrap().unwrap().len() as u64;
    let base = resident();
    let mut peak = 0;
    while let Some(chunk) = res.next_chunk().await.unwrap() {
        read += chunk.len() as u64;
        if let (Some(base), Some(now)) = (base, resident()) {
            peak = peak.max(now.saturating_sub(base));
        }
    }
    assert_eq!(read, TOTAL);
    assert!(
        peak < RSS_BOUND,
        "resident memory grew by {peak} bytes streaming {TOTAL} bytes (bound {RSS_BOUND})"
    );
}
