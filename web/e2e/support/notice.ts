// The Legal Notice version the OpenAPI mock serves by default: the `LegalNotice` schema's example
// in openapi.json, which `crates/api/src/dto/auth.rs` keeps equal to `legal::NOTICE_VERSION`.
// Specs that post or expect the version read it from here, so a raise can't leave them behind.
import { readFileSync } from 'node:fs';

const spec = JSON.parse(
	readFileSync(new URL('../../src/lib/api/openapi.json', import.meta.url), 'utf8')
) as { components: { schemas: { LegalNotice: { example: { version: number } } } } };

export const NOTICE_VERSION: number = spec.components.schemas.LegalNotice.example.version;
