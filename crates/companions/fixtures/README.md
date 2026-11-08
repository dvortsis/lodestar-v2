# Companion detection fixtures (CMP-8a)

Recorded answers for `tests/detect.rs`, one folder per companion and one per
case, in the recorded-exchange layout the source plugins use (SRC-7):

- `NNN.http`: the request line (`GET <path>`), a blank line, `HTTP <status>`
  and the response headers;
- `NNN.body`: the body, byte for byte (`.gitattributes` keeps line endings).

The test server answers each path with its recording, and the test checks
that detection asked for exactly these paths, in this order.

Sources (`docs/decisions/2026-11-08-CMP-8a.md`):

- **qBittorrent:** `403 Forbidden` from the integrations crate's 4.6.3
  recording; the login pages are upstream's `src/webui/www/public/index.html`
  at `release-4.6.3` and `release-5.0.3`, with the WebUI's substitutions
  (English, a cache id) applied.
- **Prowlarr:** `/ping` per `Prowlarr.Http/Ping/PingController.cs`; the
  login page is `frontend/src/login.html` (`develop`) trimmed to its head and
  an empty form: the title is the marker.
- **gluetun:** `/v1/vpn/status` per the control server wiki; `401` and `404`
  bodies are Go's `http.Error` text from `internal/server/middlewares/auth`.

The `wrong_service*` cases are other programs on the same port: nginx, a
router's page, Sonarr, a server that asks for authentication everywhere.
