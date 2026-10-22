# X-utp: uTP (BEP 29) transport behind a flag
**Model:** Opus (core: a protocol, untrusted input, concurrency). **Branch:** `slice/X-utp`.
**Spec:**
- `docs/owner-decisions.md` and `runner/standards.md` (both on branch `runner/standards`, PR #39, until it merges);
- architecture §6 (peer wire, MSE, concurrency budget, hostile-input limits; "TCP first (M1), uTP (BEP 29) in M7. The UI shows the success rate so the uTP gain is measurable."), §15 ("uTP maturity in Rust: ship TCP-only first and add uTP behind a flag; measure the gain"), §11 (Windows: ephemeral-port and UDP notes), §13, and §5 (DHT and peers share port 6881);
- BEP 29.

## Goal
Metadata fetches can run over uTP as well as TCP, behind a config flag that is **off by default**, with success rates measured per transport. MSE/PE must work over uTP exactly as over TCP.

## Do
1. **Pick the implementation** and record the choice in the PR and in a §16 entry.
   - Evaluate maintained Rust uTP crates. Criteria:
     - licence MIT or Apache-2.0;
     - tokio-native;
     - LEDBAT congestion control;
     - bounded memory per connection;
     - no `unsafe` needed in our code (the workspace forbids it);
     - fuzzable packet parsing;
     - active maintenance.
   - If none qualifies, implement a new crate, `crates/utp`: packet codec, connection state machine, LEDBAT, selective ACK, and timeouts with exponential backoff.
   - Either way, the packet parser is ours to fuzz. Add a `utp` fuzz target, plus property tests for the codec round-trip and the state machine (reordering, loss, duplicates, wraparound of sequence numbers).
2. **Transport abstraction in `crates/peer-wire`.**
   - The BEP 3/10/9 fetch and MSE run over any `AsyncRead + AsyncWrite` stream.
   - Add `Transport::{Tcp, Utp}` to `FetchOptions`, plus a `try_utp_first` / fallback policy. The default is TCP only.
   - Keep the existing per-IP limits, timeouts and the metadata size cap identical across transports.
3. **Shared UDP port.** uTP and the DHT share UDP 6881 (§5). Provide a demux function: a packet whose first byte has version nibble 1 and type 0–4 is uTP, and bencode (`d…`) is KRPC. Also provide a `UtpSocket` that can be fed packets from an existing socket.
   - **Don't edit `crates/dht`.** PR #32 (M4) is changing DHT code. Expose the hook, and describe the few lines needed in the DHT receive loop in the PR. The runner applies them after #32 merges.
4. **Metrics:** fetch attempts and successes per transport (`lodestar_fetch_{attempts,success}_total{transport=…}`), uTP retransmits, and connection timeouts.
5. **Tests:**
   - an in-process loopback test fetching a real info dict over uTP, both plaintext and MSE;
   - a lossy-link test (drop or reorder packets in a test socket shim);
   - fuzz and property tests as above.

## Don't
- Wire config into `crates/lodestar` (B2 owns the settings) or `crates/pipeline` (the M4 sessions are editing it). Expose the options; the runner wires them later.
- Change the default behaviour: TCP-only must stay byte-for-byte as today.

## Done when
- fmt, clippy and test pass.
- The new fuzz target runs for 60 s locally without findings.
- The existing peer-wire tests are unchanged and pass.
- The PR states the measured loopback throughput and the memory per connection.
