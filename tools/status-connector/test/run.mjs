// Runs the real handler with GitHub mocked. Uses this repository's own runner/slices.yaml, so it needs no network.
import { build } from 'esbuild';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.join(here, '..');
await build({ entryPoints: [path.join(root, 'src/index.ts')], bundle: true, format: 'esm', outfile: path.join(here, 'out.mjs'), platform: 'neutral', mainFields: ['module', 'main'], loader: { '.json': 'json' }, logLevel: 'error' });
const { default: worker } = await import(path.join(here, 'out.mjs') + '?' + Date.now());
const slicesYaml = fs.readFileSync(path.join(root, '../../runner/slices.yaml'), 'utf8');

// What each kind of agent leaves on GitHub.
const pr = (number, title, ref, body, merged) => ({ number, title, head: { ref }, body, merged_at: merged ? '2026-11-01T12:00:00Z' : null, user: { login: 'someone' } });
let github = {
  closed: [
    pr(1, 'B0: Split the API router', 'slice/B0', 'Generated with [Claude Code]', true), // Claude, merged
    pr(2, 'SRC-4: Consent framework', 'codex/src-4', 'Implements SRC-4.', true),         // another agent, merged
  ],
  open: [pr(3, 'SRC-5: Per-plugin egress', 'agent-x/egress', '', false)],                // open PR
  refs: [{ ref: 'refs/heads/slice/SRC-9' }],                                             // branch only
  fail: false,
};
globalThis.fetch = async (url) => {
  const u = new URL(url); const p = u.pathname; const json = (o) => new Response(JSON.stringify(o));
  if (github.fail) return new Response('rate limited', { status: 403 });
  if (p.endsWith('/contents/runner/slices.yaml')) return new Response(slicesYaml);
  if (p.endsWith('/contents/state.json')) return new Response('missing', { status: 404 }); // optional
  if (p.endsWith('/pulls') && u.searchParams.get('state') === 'open') return json(github.open);
  if (p.endsWith('/pulls')) return json(u.searchParams.get('page') === '1' ? github.closed : []);
  if (p.includes('/git/matching-refs')) return json(github.refs);
  if (p.endsWith('/commits/main')) return json({ sha: 'abcdef0123456789', commit: { committer: { date: '2026-11-02T00:00:00Z' } } });
  return new Response('not found', { status: 404 });
};
const env = { GITHUB_TOKEN: 't', GITHUB_REPO: 'o/r', PATH_SECRET: 's3cret' }; // default 60 s cache
const call = (body, p = '/mcp/s3cret', method = 'POST') => worker.fetch(new Request('https://x' + p, { method, body: method === 'POST' ? JSON.stringify(body) : undefined }), env);
const rpc = async (body) => (await call(body)).json();
const tool = async (name) => { const r = (await rpc({ jsonrpc: '2.0', id: 9, method: 'tools/call', params: { name, arguments: {} } })).result; return { isError: r.isError, rows: r.isError ? r.content[0].text : JSON.parse(r.content[0].text) }; };

assert.equal((await call({}, '/mcp/wrong')).status, 404, 'wrong secret');
assert.equal((await call(null, '/mcp/s3cret', 'GET')).status, 405, 'GET refused');
assert.equal((await call({ jsonrpc: '2.0', method: 'notifications/initialized' })).status, 202, 'notification');
assert.equal((await rpc({ jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2025-06-18' } })).result.serverInfo.name, 'lodestar-status');
assert.deepEqual((await rpc({ jsonrpc: '2.0', id: 2, method: 'tools/list' })).result.tools.map((t) => t.name), ['plan_nodes', 'plan_road', 'plan_activity', 'plan_recent', 'plan_meta']);

const nodes = (await tool('plan_nodes')).rows; const by = Object.fromEntries(nodes.map((n) => [n.id, n]));
assert.equal(by.B0.state, 'done'); assert.equal(by.B0.agent, 'claude');
assert.equal(by['SRC-4'].state, 'done'); assert.equal(by['SRC-4'].agent, 'other', 'another agent is not shown as Claude');
assert.equal(by['SRC-5'].state, 'active', 'open pull request');
assert.equal(by['SRC-9'].state, 'active', 'branch only');
assert.ok(!['done', 'closed'].includes(by['SRC-7'].state), 'SRC-7 waits on owner answers');
assert.ok(by['SRC-7'].why && by['SRC-7'].why.includes('ext:owner-q2'), 'the owner question is named as the reason');
assert.equal(by.root.level, 'root'); assert.ok(by.root.total > 100);
assert.equal((await tool('plan_road')).rows.length, 25);
const recent = (await tool('plan_recent')).rows; assert.equal(recent.length, 2); assert.equal(recent.find((r) => r.pr === 2).agent, 'other');
assert.equal((await tool('plan_meta')).rows.find((r) => r.key === 'main').value, 'abcdef01');

github.fail = true; // a GitHub failure is reported to the caller, not hidden
const failed = (await (await worker.fetch(new Request('https://x/mcp/s3cret', { method: 'POST', body: JSON.stringify({ jsonrpc: '2.0', id: 5, method: 'tools/call', params: { name: 'plan_meta' } }) }), { ...env, CACHE_SECONDS: '0' })).json()).result;
assert.equal(failed.isError, true); assert.match(failed.content[0].text, /Could not read GitHub/);
console.log('status connector: all checks passed');
