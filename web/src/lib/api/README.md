# lib/api

Typed API access. `openapi.json` is written by the Rust generator; `npm run api:types` turns it into `schema.d.ts` (types only). Use only the generated types; a missing field is a request to the main session, never a local type. Generated files are exempt from the lint bans.
