# X-cmp-supervisor-bind decisions

A follow-up of #867 (X-cmp01-fix). A companion Lodestar runs (the managed path) is now reached through the same client as the watch and the guided path. A request to it that carries anything goes out only while the child Lodestar started on its port is running and has been found ready. This file says what the code now does (`docs/security/v1/README.md` §5).

## What a managed companion's port gets
- **A managed port** is the loopback port of a companion Lodestar started. It's managed from the start of the companion, and stays managed for the rest of the run, running or not.
- **Every request to a managed port goes to `127.0.0.1` alone.** It's never looked for on `::1`: not by a bare probe, not after a stop, not after a restart. #867's per-port memory doesn't apply to it.
- **A request that carries anything** (a header, so a key or a cookie, a body, or a query: #867's "not bare" rule) goes out only when both of these hold:
  - the child Lodestar started on that port is running (`Running`, or `Unhealthy`, which is a running process whose checks fail);
  - it has been found ready in this run of it.
  Otherwise the request fails as unreachable before anything is sent.
- **Ready** means a health check on `127.0.0.1:<port>` passed while that same run was the one running. The health check of `Supervised` marks it ready. An answer while the child isn't running (starting, backing off, held, stopped) never counts, and nothing on `::1` is ever asked.
- **A restart holds credentialed requests.** When the child exits, the port isn't ready. The new child is a new run, which isn't ready until a health check finds it so. The manager already re-checks health after a restart before it configures (`follow_from`), and that check marks the new run ready.
- **A stop or a removal** releases the child. The port stays managed and isn't ready again until a new start is found ready.
- **A bare probe to a managed port** goes to `127.0.0.1` whatever the child's state, so health checks and detection work as before.

## The binding mechanism
- **Chosen: a check of the child's state at send time, plus a run number.**
  - The supervisor counts the starts of each companion's process (`Status::run`, one more each time it reaches `Running`).
  - `Bindings` (`crates/companions/src/supervise/bind.rs`) holds, for each managed port, the child's status channel and the run last found ready. The client reads it before each send.
- **Why this one:**
  - The supervisor already publishes the child's state on a watch channel, so the check is a lock and a read, with no new task and no polling.
  - The run number tells two runs of the same port apart. "Running" alone can't: a restart between a health check and a send would let the old run's readiness stand for the new one.
  - It works the same on Linux, Windows and macOS.
- **Not chosen:**
  - **Asking the OS who owns the socket** (the listening process's id) would also catch a process on `127.0.0.1` itself. But it's a different API on each platform, it needs privileges on some, and it would be a lookup per request.
  - **A token the child must return** isn't something the companions (Prowlarr, qBittorrent and the rest) can do: they're unmodified upstream programs.
- **What it leaves open:**
  - Another process that holds `127.0.0.1:<port>` before the child starts isn't told apart from the child. The child then fails to bind and exits, which ends its run before a health check can find it ready in most cases. That's a process already running on this machine as a local user, beyond what this slice addresses.
  - A request checked just before a restart can still go out as the child exits. The window is the time between the check and the connect.

## One client for every path
- **The three paths share one `CompanionHttp`:** the managed path (`Api` and `Supervised`), the watch over the companions you added, and the guided path. `companions::start` builds it with the `Bindings` and hands it to `companion_wiring::serve`. The watch and the guided path no longer build their own.
- **Each path benefits from the others' probes,** as #867's notes expected. A hand-run companion on another port (the guided path) behaves exactly as after #867: a bare probe may find it on `::1`, and what carries something follows.
- **If the client can't be built,** the managed path, the watch and the guided path are all off for the run, logged as `companion_http`, as each was before when its own client failed.

## An entered `localhost`
- **An entered URL naming `localhost`** (any case, with or without the trailing dot) resolves to `127.0.0.1` alone, whatever the system resolver says. A companion you run yourself and enter as `http://localhost:<port>` is reached only on IPv4 loopback.
- **Not chosen:** sending entered loopback URLs through the per-port rule. That would be a larger change to the entered path, and the name is the only way such a URL reaches `::1` without saying so. A URL that names `[::1]` itself still goes there, because you asked for it.

## Tests
- **Real listeners, as #867's tests use** (`crates/companions/tests/detect.rs`, module `bound`):
  - a `::1`-only impostor on a managed port, before the child listens and after it's running, gets nothing, a bare probe included;
  - a restart holds credentialed requests until the new run is found ready, and readiness from the old run counts for nothing; a stop holds them;
  - a hand-run companion on an unmanaged port is reached as after #867 through a client with bindings;
  - an entered `localhost` reaches `127.0.0.1` even when the resolver offers only `::1`.
- **`Supervised`** (`tests/supervised.rs`): a companion held for its route owns its port from its start. An answer on `127.0.0.1` from another process lets no credentialed request through and never passes its health check, and the port stays managed after the stop.
- **Unit tests** in `bind.rs` cover the run rule and release.
- Where `::1` can't be bound, the `::1` tests say so and pass. On CI (`CI` set) they fail instead, as #867's do.
