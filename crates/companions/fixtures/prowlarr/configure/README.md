# Prowlarr configuration fixtures (CMP-8b)

`host.json` is the starting state of the fake Prowlarr in
`tests/configure.rs`: `GET /api/v1/config/host` answers it, and
`PUT /api/v1/config/host/1` replaces it. Its fields are
`HostConfigResource.cs` on `develop`, with a fresh install's values and a
placeholder password hash; enums are camel-case strings, as Prowlarr's
JSON options write them.

Reconstructed from upstream source, not captured from a pinned image: the
catalogue pins none yet (`docs/decisions/2026-11-08-CMP-8b.md`).
