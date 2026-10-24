//! The test binary's one log capture: the process's global `tracing`
//! subscriber, installed once, writing every thread's events to a buffer.
//! A thread-scoped subscriber races other threads over each callsite's
//! cached interest and can miss an event, so tests that read the log use
//! this one; each looks for strings spelled so no other test logs them.

use std::sync::{Mutex, OnceLock};

use tracing_subscriber::fmt::MakeWriter;

static LOG: Mutex<Vec<u8>> = Mutex::new(Vec::new());

struct Buf;

impl std::io::Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        LOG.lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct MakeBuf;

impl<'a> MakeWriter<'a> for MakeBuf {
    type Writer = Buf;
    fn make_writer(&'a self) -> Buf {
        Buf
    }
}

/// Installs the capture (once per process).
pub fn install() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .with_writer(MakeBuf)
            .finish();
        tracing::subscriber::set_global_default(subscriber).expect("no other global");
    });
}

/// Everything logged since [`install`], by every test.
pub fn text() -> String {
    String::from_utf8_lossy(&LOG.lock().unwrap_or_else(|e| e.into_inner())).into_owned()
}
