//! What the binary's tests share.
//!
//! **A server binds `127.0.0.1:0` and says where it is.** Picking a free
//! port first (bind port 0, read it, drop it, start the server on it)
//! races every other test doing the same: two servers can be handed the
//! same number, and one test then talks to another's server. So each
//! server binds port 0 itself, and the test reads the port it got from
//! its `app.serving` line, on the console or in either log file.

/// The bind to give a server: loopback, any port.
pub const ANY_PORT: &str = "127.0.0.1:0";

/// The port in the last `app.serving` line of `log` (the console's text,
/// `lodestar.log` or `lodestar.jsonl`), if one is there yet. The last: a
/// server that restarts binds a new port.
pub fn serving_port(log: &str) -> Option<u16> {
    let line = log.lines().rev().find(|l| l.contains("app.serving"))?;
    let after = line.split("127.0.0.1:").nth(1)?;
    let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok().filter(|&p| p != 0)
}
