# FlareSolverr fixtures (CMP-12)

Recorded-exchange cases for `tests/flaresolverr.rs`, in the layout of
`../README.md` (`NNN.http` and `NNN.body`), and Prowlarr's answers for the
indexer-proxy pairing in `configure/`.

They are reconstructed from upstream source, not captured from a pinned
image: the catalogue pins none yet (`docs/decisions/2026-11-12-CMP-12.md`).

- **found:** `GET /` and `GET /health` per FlareSolverr's
  `src/flaresolverr.py` (`index_endpoint`, `health_endpoint`), v3.3.21.
  `found_v2` is a v2.2 answer, whose user agent is Firefox's.
- **unhealthy:** a ready index and a `500` from `/health`.
- **wrong_service:** nginx's default page on the same port.
- **configure/indexerproxy.json:** `GET /api/v1/indexerproxy` with two
  proxies of your own: a FlareSolverr one tagged `flaresolverr` and an HTTP
  proxy (Prowlarr's `IndexerProxyResource`, fields as `FlareSolverrSettings`
  and `HttpSettings` declare them).
- **configure/tag.json:** `GET /api/v1/tag` with your own tag.
