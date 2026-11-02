// Runs the real handler with GitHub mocked and a small fixture slice graph, so it needs no network and does not depend on the live plan.
import { build } from 'esbuild';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.join(here, '..');
await build({ entryPoints: [path.join(root, 'src/index.ts')], bundle: true, format: 'esm', outfile: path.join(here, 'out.mjs'), platform: 'neutral', mainFields: ['module', 'main'], loader: { '.json': 'json' }, logLevel: 'error' });
const { default: worker } = await import(path.join(here, 'out.mjs') + '?' + Date.now());
const slicesYaml = fs.readFileSync(path.join(here, 'fixture-slices.yaml'), 'utf8');
const meta = JSON.parse(fs.readFileSync(path.join(root, 'src/meta.json'), 'utf8'));

// What each kind of agent leaves on GitHub.
const pr = (number, title, ref, body, merged) => ({ number, title, head: { ref }, body, merged_at: merged ? '2026-11-01T12:00:00Z' : null, user: { login: 'someone' } });
let github = {
  closed: [
    pr(1, 'B0: Split the API router', 'slice/B0', 'Generated with [Claude Code]', true), // Claude, merged
    pr(2, 'SRC-4: Consent framework', 'codex/src-4', 'Implements SRC-4.', true),         // another agent, merged
    pr(4, 'SRC-6/SRC-8: Parsers', 'agent-y/parsers', '', true),                          // one PR, two slices
    pr(5, 'SRC-12: Scrape', 'slice/SRC-12', '', false),                                  // closed, never merged
  ],
  open: [pr(3, 'SRC-5: Per-plugin egress', 'agent-x/egress', '', false)],                // open PR
  refs: [{ ref: 'refs/heads/slice/SRC-9' }, { ref: 'refs/heads/slice/SRC-12' }],            // branch only; SRC-12's PR was closed unmerged
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
const SECRET = 'a'.repeat(40);
const env = { GITHUB_TOKEN: 't', GITHUB_REPO: 'o/r', PATH_SECRET: SECRET }; // default 60 s cache
const call = (body, p = '/mcp/' + SECRET, method = 'POST', e = env) => worker.fetch(new Request('https://x' + p, { method, body: method === 'POST' ? JSON.stringify(body) : undefined }), e);
const rpc = async (body) => (await call(body)).json();
const tool = async (name) => { const r = (await rpc({ jsonrpc: '2.0', id: 9, method: 'tools/call', params: { name, arguments: {} } })).result; return { isError: r.isError, rows: r.isError ? r.content[0].text : JSON.parse(r.content[0].text) }; };

// the path is the password: it must be set, at least 32 characters, and exact
const ping = { jsonrpc: '2.0', id: 1, method: 'ping' };
assert.equal((await call(ping, '/mcp/wrong')).status, 404, 'wrong secret');
assert.equal((await call(ping, '/mcp/' + 'b'.repeat(40))).status, 404, 'wrong secret of the right length');
assert.equal((await call(ping, '/mcp/' + SECRET + 'x')).status, 404, 'longer than the secret');
assert.equal((await call(ping, '/mcp/undefined', 'POST', { ...env, PATH_SECRET: undefined })).status, 404, 'unset secret serves nothing, not /mcp/undefined');
assert.equal((await call(ping, '/mcp/', 'POST', { ...env, PATH_SECRET: '' })).status, 404, 'empty secret serves nothing, not /mcp/');
assert.equal((await call(ping, '/mcp/short', 'POST', { ...env, PATH_SECRET: 'short' })).status, 404, 'a short secret is refused');
assert.equal((await call(ping)).status, 200, 'the right secret works');
assert.equal((await call(null, '/mcp/' + SECRET, 'GET')).status, 405, 'GET refused');
assert.equal((await call(null)).status, 200, 'a null body is a JSON-RPC error, not a crash');
assert.equal((await (await call(null)).json()).error.code, -32600);
assert.equal((await call({ jsonrpc: '2.0', method: 'notifications/initialized' })).status, 202, 'notification');
assert.equal((await rpc({ jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '1999-01-01' } })).result.protocolVersion, '2025-06-18', 'only a supported protocol version is answered');
assert.equal((await rpc({ jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2025-06-18' } })).result.serverInfo.name, 'lodestar-status');
assert.deepEqual((await rpc({ jsonrpc: '2.0', id: 2, method: 'tools/list' })).result.tools.map((t) => t.name), ['plan_nodes', 'plan_road', 'plan_activity', 'plan_recent', 'plan_meta']);

const nodes = (await tool('plan_nodes')).rows; const by = Object.fromEntries(nodes.map((n) => [n.id, n]));
assert.equal(by.B0.state, 'done'); assert.equal(by.B0.agent, 'claude');
assert.equal(by['SRC-4'].state, 'done'); assert.equal(by['SRC-4'].agent, 'other', 'another agent is not shown as Claude');
assert.equal(by['SRC-5'].state, 'active', 'open pull request');
assert.equal(by['SRC-9'].state, 'active', 'branch only');
assert.equal(by['SRC-6'].state, 'done'); assert.equal(by['SRC-8'].state, 'done', 'one PR can name two slices');
assert.notEqual(by['SRC-12'].state, 'active', 'a branch whose only PR was closed unmerged is not work in progress');
assert.ok(!['done', 'closed'].includes(by['SRC-7'].state), 'SRC-7 waits on owner answers');
assert.ok(by['SRC-7'].why.includes('ext:owner-q2') && by['SRC-7'].why.includes('ext:owner-q15'), 'the owner questions are named as the reason');
assert.equal(by.root.level, 'root'); assert.equal(by.root.total, nodes.filter((n) => n.level === 'slice').length);
assert.equal((await tool('plan_road')).rows.length, meta.road.length);
const recent = (await tool('plan_recent')).rows; assert.equal(recent.length, 3); assert.equal(recent.find((r) => r.pr === 2).agent, 'other');
assert.equal((await tool('plan_meta')).rows.find((r) => r.key === 'main').value, 'abcdef01');

// a full first page means there may be more: the next page is asked for, so older merged slices do not drop out
const pagesAsked = []; const realFetch = globalThis.fetch;
github.closed = Array.from({ length: 100 }, (_, i) => pr(100 + i, 'X-' + i + ': filler', 'x', '', true)); github.closed2 = [pr(900, 'B0: Split the API router', 'slice/B0', '', true)];
globalThis.fetch = async (url, init) => { const u = new URL(url); if (u.pathname.endsWith('/pulls') && u.searchParams.get('state') === 'closed') { const n = +u.searchParams.get('page'); pagesAsked.push(n); return new Response(JSON.stringify(n === 1 ? github.closed : n === 2 ? github.closed2 : [])); } return realFetch(url, init); };
await (await call({ jsonrpc: '2.0', id: 8, method: 'tools/call', params: { name: 'plan_meta' } }, '/mcp/' + SECRET, 'POST', { ...env, CACHE_SECONDS: '0' })).json();
assert.ok(pagesAsked.includes(2), 'page 2 of closed pull requests is read when page 1 is full');
globalThis.fetch = realFetch; github.closed = [];

github.fail = true; // a GitHub failure is reported to the caller, not hidden
const failed = (await (await worker.fetch(new Request('https://x/mcp/' + SECRET + '', { method: 'POST', body: JSON.stringify({ jsonrpc: '2.0', id: 5, method: 'tools/call', params: { name: 'plan_meta' } }) }), { ...env, CACHE_SECONDS: '0' })).json()).result;
assert.equal(failed.isError, true); assert.match(failed.content[0].text, /Could not read GitHub/);
console.log('status connector: all checks passed');
