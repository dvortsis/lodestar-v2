// openapi.json -> schema.d.ts (types only). No-op if the JSON hasn't been generated yet.
import { existsSync } from 'node:fs';
import { spawnSync } from 'node:child_process';

const input = 'src/lib/api/openapi.json';
if (!existsSync(input)) {
	console.log(`api:types: ${input} not found, skipping`);
	process.exit(0);
}
const bin = process.platform === 'win32' ? 'npx.cmd' : 'npx';
const r = spawnSync(bin, ['openapi-typescript', input, '-o', 'src/lib/api/schema.d.ts'], {
	stdio: 'inherit'
});
process.exit(r.status ?? 1);
