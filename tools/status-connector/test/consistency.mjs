// Checks that runner/dashboard.yaml and runner/slices.yaml agree, so a plan change can't leave the dashboard behind.
// Run by `npm test`, and by CI on any change to either file.
import { build } from 'esbuild';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { parse } from 'yaml';

const here = path.dirname(fileURLToPath(import.meta.url));
await build({ entryPoints: [path.join(here, '../src/plan.ts')], bundle: true, format: 'esm', outfile: path.join(here, 'plan.out.mjs'), platform: 'neutral', logLevel: 'error' });
const { buildPlan } = await import(path.join(here, 'plan.out.mjs') + '?' + Date.now());
const runner = path.join(here, '../../../runner');
const slices = parse(fs.readFileSync(path.join(runner, 'slices.yaml'), 'utf8'));
const cfg = parse(fs.readFileSync(path.join(runner, 'dashboard.yaml'), 'utf8'));
const plan = buildPlan({ slices: slices.slices, external: slices.external || {}, cfg, runnerState: null, mergedPRs: [], openPRs: [], branches: [], mainSha: '00000000', mainDate: '', now: new Date().toISOString() });
const problems = plan.meta.filter((r) => r.key === 'warning').map((r) => `${r.label}: ${r.value}. ${r.note}`);
assert.deepEqual(problems, [], 'runner/dashboard.yaml is out of step with runner/slices.yaml. Fix it in the same PR (see "Keeping the dashboard current" in CLAUDE.md):\n  ' + problems.join('\n  '));
for (const k of ['phases', 'workstreams', 'placement', 'conditions', 'roadmap', 'gates', 'owner_queue', 'notes', 'crawler', 'soak']) assert.ok(cfg[k], 'dashboard.yaml is missing ' + k);
console.log(`dashboard.yaml agrees with slices.yaml: ${slices.slices.length} slices, ${plan.road.length} roadmap steps, ${plan.gates.length} gates, ${Object.keys(cfg.conditions).length} conditions`);
