# X-cmp-egress-prelaunch decisions

A supervised companion never sends traffic outside its chosen route, not even in the seconds before CMP-8b configures it (#660's review, `reviews/rv-660-c54df896.md` on runner-state; plan Q8, Q18; architecture §18.4, §18.10). These are the slice's decisions under §2.33. None loosens an owner decision or a security decision. The runner flags them to the owner (§2.19).

## The window
- **What #660 left:** a companion on a SOCKS5 route (Prowlarr; qBittorrent and FlareSolverr refuse SOCKS5) was started, then configured through its API. Until then its own proxy setting was empty, so its startup requests (the update check, the indexer-definition sync, telemetry) went direct. It first became reachable in CMP-5g-3, and nothing reached it yet: every catalogue pin is null.

## Why not `config.xml` (the brief's preferred way)
Checked against Prowlarr's source (`develop`, `src/`; there's no pinned version to check yet, and it targets `net10.0` per its `.csproj` files and `global.json`):
- **Prowlarr's proxy isn't a `config.xml` setting.** `ConfigFileProvider.cs`, which reads `config.xml`, has no proxy key. `ProxyEnabled`, `ProxyType`, `ProxyHostname`, `ProxyPort` and the rest are properties of `ConfigService.cs`, read from its SQLite `Config` table through `IConfigRepository`. So nothing written into `config.xml` sets it. Writing its database before the first start isn't an option either: its migrations create it, and its schema is Prowlarr's own.

## What's done instead: the route is in the launch environment
- **What Prowlarr reads at boot:** with its own proxy setting empty, `ManagedHttpDispatcher.CreateHttpClient` builds a `SocketsHttpHandler` with no `Proxy` and `UseProxy` at its default (`true`). .NET then uses `HttpClient.DefaultProxy`.
  - That reads `ALL_PROXY`, `HTTP_PROXY`, `HTTPS_PROXY` and `NO_PROXY` first on every platform: `SystemProxyInfo.Unix.cs`, and `SystemProxyInfo.Windows.cs`, which tries `HttpEnvironmentProxy.TryCreate` before the system settings (dotnet/runtime, `main` and `release/8.0`).
  - It accepts `socks5://`, and `SocksHelper.cs` sends a host name to a SOCKS5 proxy as a name, so the proxy resolves it.
  - Prowlarr's code has one `SocketsHttpHandler` (that dispatcher), no `UseProxy = false`, and no other `HttpClient` outside its tests. Sentry's client takes `DefaultProxy` too, so the variables cover it, which Prowlarr's own proxy setting doesn't.
- **So every start of a companion on a SOCKS5 route** is launched with `ALL_PROXY`, `HTTP_PROXY` and `HTTPS_PROXY` set to `socks5://<proxy>`, and `NO_PROXY=localhost,127.0.0.1,::1` (`manager::launch_env`). On Unix the lower-case forms are set too, to the same values, so the result doesn't depend on which one .NET's Unix reader checks first (it checks lower case first); Windows' names aren't case sensitive (runner, 2026-11-11). A direct route adds none of them. Unit tests assert both exactly.
  - That's the same proxy it's then configured for (`manager::Prelaunch`, built with the route by `CompanionRoutes`).
  - It's set from the first instruction and stays for the process's life, not only until it's configured.
  - Loopback alone is bypassed; it never leaves the machine (its own WebUI, other companions here).
  - The supervisor still clears every other variable.
- **CMP-8b's configuration still sets Prowlarr's own proxy** after the start, as before. Once set, it takes precedence for its HTTP client.
- **Fails closed:** a companion on a non-direct route whose route can't be put into its launch is not started (`route_not_expressible`): never "direct, then configured".
  - That covers a SOCKS5 route whose proxy isn't known (an unreadable setting, a proxy name that didn't resolve), which before was started and held for good.
  - It also covers interface and bind-address routes, which have no launch form yet. No companion setting can choose them: sources offer direct and SOCKS5 only.
  - A route the companion can't follow at all keeps its own refusal first (`cant_follow_route`: qBittorrent and FlareSolverr on SOCKS5).
- **Not chosen: "no network until configured".** Windows has no unprivileged way to start a process without network. Linux has one only with Landlock's TCP rules (kernel 6.7 or later), which don't cover UDP, so name lookups would still leave. It would also take a restart after every configuration.
- **Traffic outside `HttpClient`** (runner's point 2). A search of Prowlarr's source, tests aside, finds:
  - no `TcpClient`, `UdpClient`, `WebRequest.Create` or `HttpWebRequest`;
  - one `new Socket(` and six `Dns.*` calls. Each is either harmless here or a residual:
  - `HttpHappyEyeballs.OnConnect` (`Socket` and `Dns.GetHostAddressesAsync`) is the dispatcher's `ConnectCallback`. Behind a proxy, .NET uses it for the connection to the proxy, and Lodestar's proxy is an IP literal (`Socks5Proxy`), so no name is looked up.
  - `ConfigureHostFilteringOptions` calls `Dns.GetHostName()`, the machine's own name, which is no network traffic.
  - `ProxyCheck` (a health check) resolves Prowlarr's own proxy host only while that's enabled, and CMP-8b sets it to the same IP literal.
  - `RequestExtensions.GetHostName` does a reverse lookup of an incoming client's address: Lodestar on loopback here.
  - **Residual:** `X509CertificateValidationService.GetIPAddresses` resolves a site's host name with the system resolver when that site's TLS certificate fails validation, to decide whether it's a local address. So a site with a bad certificate has its name looked up outside the route; nothing is sent to it that way. This is listed, not fixed here.
- **Credentials** (runner's point 3): the route carries none today. CMP-8b's `Socks5Proxy` is an address and a port, and a source's SOCKS5 setting saves its credentials apart (SRC-4), never into the companion's row. So the launch value is `socks5://<ip>:<port>`, with no secret in it. The launch environment is never logged: the supervisor emits no log events, `SpawnSpec`'s `Debug` shows variable names only, and `Supervised`'s shows nothing (tested). If a route ever carries proxy credentials, the value would contain them. Then they belong in the companion's redaction secrets, and this file's successor must say so.
- **Pinning** (runner's point 4): this relies on the pinned Prowlarr's .NET version and its dispatcher. **For CMP-13, at pin time:**
  - re-check that `ProxyEnabled` still defaults to `false` in `ConfigService.cs`;
  - re-check that the dispatcher still leaves `Proxy` unset, with `UseProxy` at its default, when Prowlarr's own proxy is off, so `DefaultProxy` and its proxy variables apply;
  - re-check that the pinned .NET still reads them, `socks5://` included;
  - re-run the searches above. The runner carries this into CMP-13's `slices.yaml` notes.

## #660's L1: the follower waits for the slot
- A companion that has to be configured once it runs, while an operation holds its slot, used to be left running unconfigured. The follower now waits for the slot, polling every 250 ms, for as long as its run is current.
  - If an operation begins a new run (any start or stop the manager makes), that operation owns the companion, and the follower ends.
  - Once the slot is free, it checks again that its run is still current, configures the companion, and keeps the slot through any stop.
  - If the slot is still held after `LIVE_DEADLINE` (5 min), the companion is stopped and shows unhealthy (Q8), logged as `companions.install_failed`, step `configure`, reason `busy`.
- Tested with an update still downloading when the companion starts running. The update fails before it touches the companion, and the companion is then configured and shows healthy.

## Deadline
- `configure_now`, every configuration the manager makes after a start, is bounded by `LIVE_DEADLINE`; past it, it fails `timed_out`. That includes the follower's configuration, which had no bound before.

## Tests
- `crates/companions/tests/supervised.rs`: the launch environment for SOCKS5 (each variable, IPv6 in brackets), for direct (none), and for kinds with no launch form (refused). A held Prowlarr is launched with its proxy, read back from the spec the sandbox passes on. A SOCKS5 route with no known proxy isn't started.
- `crates/companions/tests/manager.rs`: the L1 case above.
- `crates/lodestar/src/companions.rs`: `CompanionRoutes` hands the proxy it configures for to the launch, and none for a route it couldn't describe.
- **Not tested:** `configure_now`'s deadline. Exercising it would need a five-minute wait, or a paused clock, which the binary's tests can't use (tokio `test-util` stays out of `lodestar`).
