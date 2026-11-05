// Runs the real handler with GitHub mocked and a small fixture slice graph, so it needs no network and does not depend on the live plan.
import { build } from 'esbuild';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import { parse } from 'yaml';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.join(here, '..');
await build({ entryPoints: [path.join(root, 'src/index.ts')], bundle: true, format: 'esm', outfile: path.join(here, 'out.mjs'), platform: 'neutral', mainFields: ['module', 'main'], loader: { '.json': 'json' }, logLevel: 'error' });
const { default: worker } = await import(path.join(here, 'out.mjs') + '?' + Date.now());
const slicesYaml = fs.readFileSync(path.join(here, 'fixture-slices.yaml'), 'utf8');
const dashYaml = fs.readFileSync(path.join(here, 'fixture-dashboard.yaml'), 'utf8');
const fixtureCfg = parse(dashYaml);

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
  tags: [], tagsFail: false, page2Fail: false,
  state: { updated_at: '2026-11-02T00:00:00Z', slots: { updated: '2020-01-01T00:00:00Z', items: [
    { slot: 1, slice: 'SRC-9', role: 'build', agent: 'claude', doing: 'Parsing Internet Archive responses.' },
    { slot: 2, slice: 'NOPE-3', doing: 'Names a slice that does not exist.' },
    { slot: 2, slice: 'B0', doing: 'A second entry for slot 2.' },
    { slot: 3, slice: 'SRC-9', role: 'wizard', doing: 'The same slice again, with a role outside the set.' },
    { slot: 1.5, slice: 'SRC-4', doing: 'Fractional slot number.' },
    { slot: 9, slice: 'SRC-4', doing: 'Above the limit.' },
    { slot: 1e9, slice: 'SRC-4', doing: 'A typo that must not cost memory.' },
    { slot: 4, slice: { not: 'a string' }, doing: 42, since: ['x'], role: { r: 1 } },
  ] } },
  refs: [{ ref: 'refs/heads/slice/SRC-9' }, { ref: 'refs/heads/slice/SRC-12' }],            // branch only; SRC-12's PR was closed unmerged
  fail: false,
};
globalThis.fetch = async (url) => {
  const u = new URL(url); const p = u.pathname; const json = (o) => new Response(JSON.stringify(o));
  if (github.fail) return new Response('rate limited', { status: 403 });
  if (p.endsWith('/contents/runner/slices.yaml')) return new Response(slicesYaml);
  if (p.endsWith('/contents/runner/dashboard.yaml')) return new Response(dashYaml);
  if (p.includes('/git/matching-refs/tags/')) return github.tagsFail ? new Response('boom', { status: 500 }) : json(github.tags);
  if (p.includes('/git/matching-refs/heads/release/')) return json([]);
  if (p.endsWith('/contents/state.json')) return github.state ? new Response(JSON.stringify(github.state)) : new Response('missing', { status: 404 }); // optional
  if (p.endsWith('/pulls') && u.searchParams.get('state') === 'open') return json(github.open);
  if (p.endsWith('/pulls') && github.page2Fail && u.searchParams.get('page') === '2') return new Response('timeout', { status: 522 });
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
assert.deepEqual((await rpc({ jsonrpc: '2.0', id: 2, method: 'tools/list' })).result.tools.map((t) => t.name), ['plan_nodes', 'plan_road', 'plan_edges', 'plan_activity', 'plan_recent', 'plan_meta', 'plan_gates', 'plan_owner', 'plan_ext', 'plan_notes', 'plan_crawler', 'plan_soak', 'plan_slots']);

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
assert.equal((await tool('plan_road')).rows.length, fixtureCfg.roadmap.steps.length);
assert.equal(by['ZZ-new'].parent, 'ws-small', 'a slice no rule covers lands in the follow-ups workstream');
const warnings = (await tool('plan_meta')).rows.filter((r) => r.key === 'warning').map((r) => r.label + ':' + r.value);
assert.ok(warnings.includes('Slices not placed:1'), 'the unplaced slice is reported');
assert.ok(warnings.includes('Unknown slice:NOPE-1'), 'a roadmap step naming a missing slice is reported');
assert.ok(warnings.some((w) => w.startsWith('Unknown roadmap step')), 'an arrow naming a missing step is reported');
assert.ok(warnings.includes('Condition not described:undeclared'), 'an ext: condition the file does not describe is reported');
assert.ok(warnings.includes('Unknown slice:NOPE-2'), 'a pinned id that is not in slices.yaml is reported');
assert.ok(warnings.includes('Condition unused:stale-cond'), 'a described condition nothing uses is reported');
assert.ok(warnings.some((w) => w.startsWith('Bad placement pattern')), 'a regex that does not compile is reported, and does not stop the refresh');
assert.equal(by['SRC-13'].state, 'waiting', 'a condition slices.yaml names but dashboard.yaml lacks counts as unmet');
const gates = (await tool('plan_gates')).rows; assert.equal(gates.find((g) => g.id === 'G1').status, 'passed', 'a gate follows its slice'); assert.equal(gates.find((g) => g.id === 'G2').status, 'not started');
assert.equal((await tool('plan_owner')).rows.length, 1); assert.equal((await tool('plan_edges')).rows.length, 1, 'an arrow to a missing step is dropped');
const notes = (await tool('plan_notes')).rows; assert.deepEqual(notes.map((n) => n.group), ['now', 'you']); assert.equal(notes[0].asof, '2026-11-02');
assert.equal((await tool('plan_crawler')).rows.length, 2); assert.equal((await tool('plan_soak')).rows.length, 1);
const slots = (await tool('plan_slots')).rows;
assert.deepEqual(slots.map((x) => x.status), ['working', 'working', 'working', 'working', 'finishing'], 'rows 1..limit, then a listed slot above the limit as finishing (the limit can be lowered while a session still runs)');
assert.equal(slots[4].slot, 9);
assert.equal(slots[3].slice, '', 'a value that is not a string is dropped, not passed through'); assert.equal(slots[3].doing, '42'); assert.equal(slots[3].since, '');
assert.equal(slots[0].slice, 'SRC-9'); assert.ok(slots[0].title && slots[0].stage, 'a slot takes its title and state from the slice graph');
assert.equal(slots[1].slice, 'NOPE-3', 'the first entry for a slot wins');
assert.equal(slots[2].role, 'build', 'a role outside the set falls back to build');
for (const w of ['Unknown slice:NOPE-3', 'Slot listed twice:2', 'Bad slot number:1.5', 'Slice in two slots:SRC-9', 'Bad slot value:role wizard', 'Slot above the limit:1000000000']) assert.ok(warnings.includes(w), 'reported: ' + w);
assert.ok(!warnings.includes('Slot above the limit:9'), 'a slot above the limit but within what is drawn is shown as finishing, not warned about');
assert.ok(warnings.some((w) => w.startsWith('Slots list is stale')), 'a slots list the runner has not refreshed in 24 h is reported');
// a condition with `auto:` follows GitHub: unmet until the tag exists
assert.equal((await tool('plan_ext')).rows.find((e) => e.key === 'v-tagged').met, false);
assert.equal(by['SRC-AUTO'].state, 'you', 'waits on the untagged release');
const recent = (await tool('plan_recent')).rows; assert.equal(recent.length, 3); assert.equal(recent.find((r) => r.pr === 2).agent, 'other');
assert.equal((await tool('plan_meta')).rows.find((r) => r.key === 'main').value, 'abcdef01');

// an `auto:` condition follows GitHub: no such branch means it is unmet, even though the file says met
assert.equal((await tool('plan_ext')).rows.find((e) => e.key === 'rel-branch').met, false);
// if GitHub can't answer, the file's value stands instead of flipping to unmet
github.tagsFail = true;
const tagless = await call({ jsonrpc: '2.0', id: 6, method: 'tools/call', params: { name: 'plan_ext', arguments: {} } }, '/mcp/' + SECRET, 'POST', { ...env, CACHE_SECONDS: '0' });
assert.equal(JSON.parse((await tagless.json()).result.content[0].text).find((e) => e.key === 'rel-branch').met, true, 'a refs failure keeps the file value');
github.tagsFail = false;

// once the tag exists the condition holds, with no edit to any file
github.tags = [{ ref: 'refs/tags/v9.9.9' }];
// (another repo name, so the 30 s memory of the failed refs call above does not apply)
const after = await call({ jsonrpc: '2.0', id: 7, method: 'tools/call', params: { name: 'plan_nodes', arguments: {} } }, '/mcp/' + SECRET, 'POST', { ...env, CACHE_SECONDS: '0', GITHUB_REPO: 'o/r2' });
const afterNodes = JSON.parse((await after.json()).result.content[0].text);
assert.equal(afterNodes.find((n) => n.id === 'SRC-AUTO').state, 'ready', 'a tag appearing on GitHub releases what waited on it');
github.tags = [];

// a full first page means there may be more: the next page is asked for, so older merged slices do not drop out
const pagesAsked = []; const realFetch = globalThis.fetch;
github.closed = Array.from({ length: 100 }, (_, i) => pr(100 + i, 'X-' + i + ': filler', 'x', '', true)); github.closed2 = [pr(900, 'B0: Split the API router', 'slice/B0', '', true)];
globalThis.fetch = async (url, init) => { const u = new URL(url); if (u.pathname.endsWith('/pulls') && u.searchParams.get('state') === 'closed') { const n = +u.searchParams.get('page'); pagesAsked.push(n); return new Response(JSON.stringify(n === 1 ? github.closed : n === 2 ? github.closed2 : [])); } return realFetch(url, init); };
await (await call({ jsonrpc: '2.0', id: 8, method: 'tools/call', params: { name: 'plan_meta' } }, '/mcp/' + SECRET, 'POST', { ...env, CACHE_SECONDS: '0' })).json();
assert.ok(pagesAsked.includes(2), 'page 2 of closed pull requests is read when page 1 is full');
globalThis.fetch = realFetch; github.closed = [];

// one timed-out page of older closed pull requests must not take every tool down: the plan is built without it and says so
github.page2Fail = true;
const metaOf = async (repo) => JSON.parse((await (await call({ jsonrpc: '2.0', id: 11, method: 'tools/call', params: { name: 'plan_meta', arguments: {} } }, '/mcp/' + SECRET, 'POST', { ...env, CACHE_SECONDS: '0', GITHUB_REPO: repo })).json()).result.content[0].text);
assert.ok(!(await metaOf('o/r4')).some((r) => r.label === 'Merge history incomplete'), 'a failing page after a short page is ignored: that page does not exist');
const savedClosed = github.closed; github.closed = Array.from({ length: 100 }, (_, i) => pr(5000 + i, 'X-fill-' + i, 'x/fill-' + i, '', false)); // page 1 full, so page 2 matters
const partial = await call({ jsonrpc: '2.0', id: 11, method: 'tools/call', params: { name: 'plan_meta', arguments: {} } }, '/mcp/' + SECRET, 'POST', { ...env, CACHE_SECONDS: '0', GITHUB_REPO: 'o/r3' });
const partialRes = (await partial.json()).result; assert.equal(partialRes.isError, false, 'a failed older page is not fatal');
assert.ok(JSON.parse(partialRes.content[0].text).some((r) => r.key === 'warning' && r.label === 'Merge history incomplete'), 'and it is reported');
github.page2Fail = false;
github.closed = savedClosed;
// bad slot metadata is reported, never trusted
const stateBefore = github.state;
github.state = { updated_at: 'x', slots: { updated: '2999-01-01T00:00:00Z', items: 'not a list' } };
const bad1 = (await metaOf('o/r6')).filter((r) => r.key === 'warning').map((r) => r.label);
assert.ok(bad1.includes('Slots malformed'), 'items that is not a list is reported'); assert.ok(bad1.includes('Slots date invalid'), 'a date in the future is invalid, not fresh');
github.state = { updated_at: 'x', slots: { updated: 'yesterday-ish', items: [] } };
assert.ok((await metaOf('o/r7')).some((r) => r.label === 'Slots date invalid'), 'an unparseable date says invalid, not missing');
github.state = { updated_at: 'x', slots: { items: [] } };
assert.ok((await metaOf('o/r8')).some((r) => r.label === 'Slots date missing'), 'no date says missing');
github.state = stateBefore;

// the dashboard adapts to work in flight: a branch name counts as well as a title id, and a slice with a PR or branch is a note, not an error
const toolR = async (name, repo) => JSON.parse((await (await call({ jsonrpc: '2.0', id: 12, method: 'tools/call', params: { name, arguments: {} } }, '/mcp/' + SECRET, 'POST', { ...env, CACHE_SECONDS: '0', GITHUB_REPO: repo })).json()).result.content[0].text);
const savedOpen = github.open, savedRefs = github.refs;
github.open = [
  pr(3, 'SRC-5: Per-plugin egress', 'agent-x/egress', '', false),                       // nobody's slot names it: still reported
  pr(20, 'X-brand-new: Not in slices.yaml yet', 'slice/X-brand-new', 'Generated with [Claude Code]', false), // an X-* slice slices.yaml does not list
  pr(21, 'B0-2: Follow-up to a merged slice', 'slice/B0', '', false),                    // a follow-up on a reused branch: title id differs
  pr(22, 'Y-2: Title id differs from the branch', 'slice/Y-1', '', false),
  pr(24, 'X-both: Listed by runner-state and has a PR', 'slice/X-both', '', false),       // runner-state already draws it as a round: no second node
  pr(25, 'X-a-2: Follow-up on a reused branch', 'slice/X-a', '', false),                 // title id only: one provisional node, not two
  pr(26, 'X-placed-1: A placement rule covers it', 'slice/X-placed-1', '', false),
  pr(27, 'X-done-1: Merged earlier, new PR', 'slice/X-done-1', '', false),
  pr(28, 'Bump foo from 1 to 2', 'dependabot/foo', '', false),                           // no slice id anywhere: not "work in no slot"
];
github.closed = [
  ...savedClosed,
  pr(1, 'B0: Split the API router', 'slice/B0', '', true),                            // B0 is merged, so its open follow-up is a follow-up
  pr(30, 'X-done-1: Merged', 'slice/X-done-1', '', true),
  pr(31, 'SRC-9b: Abandoned rework', 'slice/SRC-9', '', false),                        // closed, never merged: the branch's slice (SRC-9) is abandoned
  { ...pr(32, 'Bump foo from 1 to 2', 'dependabot/foo', '', false) },
];
github.refs = [{ ref: 'refs/heads/slice/Q-branch-only' }, { ref: 'refs/heads/slice/X-bare' }, { ref: 'refs/heads/slice/SRC-9' }];
github.state = { updated_at: 'x', slices: { 'X-both': { status: 'building', notes: 'in runner-state' } }, slots: { updated: new Date().toISOString(), items: [
  { slot: 1, slice: 'B0', doing: 'Follow-up on a merged slice.' },
  { slot: 2, slice: 'Y-1', doing: 'Named by its branch.' },
  { slot: 3, slice: 'X-brand-new', doing: 'Not in the plan files yet.' },
  { slot: 4, slice: 'Q-branch-only', doing: 'A branch and no PR yet.' },
  { slot: 5, slice: 'NOPE-9', doing: 'Nothing on GitHub at all.' },
  { slot: 6, slice: 'constructor', doing: 'A name every object already has.' },
  { slot: 7, slice: '__proto__', doing: 'And another.' },
] } };
const ad = await toolR('plan_meta', 'o/r9');
const adW = ad.filter((r) => r.key === 'warning').map((r) => r.label + ':' + r.value), adN = ad.filter((r) => r.key === 'note').map((r) => r.label + ':' + r.value);
assert.ok(!adW.includes('Slot lists finished work:B0'), 'a merged slice with an open follow-up PR is still being worked on');
assert.ok(!adW.some((w) => w === 'Unknown slice:X-brand-new' || w === 'Unknown slice:Q-branch-only' || w === 'Unknown slice:Y-1'), 'a slice with a PR or a branch is not an error');
assert.ok(adW.includes('Unknown slice:NOPE-9'), 'a slot naming a slice with neither a PR nor a branch is still an error');
assert.ok(adN.includes('Slot slice not in plan files yet:Q-branch-only') && adN.includes('Slot slice not in plan files yet:Y-1'), 'a slot slice that only has a branch or a PR is a quieter note');
assert.ok(adN.includes('Slice not in plan files yet:X-brand-new'), 'an X-* slice with an open PR is noted');
const openLoose = ad.find((r) => r.label === 'Open work in no slot');
assert.ok(openLoose && /SRC-5/.test(openLoose.note) && !/B0-2|Y-2|X-brand-new/.test(openLoose.note), 'a PR counts as slotted through its branch name; only the one nobody lists is reported');
const adNodes = await toolR('plan_nodes', 'o/r9'), brand = adNodes.find((n) => n.id === 'X-brand-new');
assert.ok(brand && brand.state === 'active' && brand.pr === 20 && brand.level === 'slice' && brand.parent === 'ws-small', 'an X-* slice with an open PR appears in the default lane at once');
const adSlots = await toolR('plan_slots', 'o/r9');
assert.deepEqual(adSlots.slice(0, 4).map((x) => [x.slice, x.stage, x.pr]), [['B0', 'active', 21], ['Y-1', 'active', 22], ['X-brand-new', 'active', 20], ['Q-branch-only', 'active', 0]], 'rows show the live state and the open PR');
assert.ok(!adNodes.some((n) => ['Y-1', 'Y-2', 'B0-2', 'Q-branch-only'].includes(n.id)), 'only X-* slices get a tree node');
assert.notEqual(adNodes.find((n) => n.id === 'SRC-9').state, 'active', 'a branch whose only PR was closed unmerged (under another title id) is abandoned, not active');
assert.ok(!openLoose.note.includes('#28') && !/Bump/.test(openLoose.note), 'an open PR with no slice id is not work in no slot');
assert.equal(adSlots.find((x) => x.slot === 6)?.slice, 'constructor', 'a slot slice named like an Object property is handled, not a crash');
assert.deepEqual(adNodes.filter((n) => n.id === 'X-both').map((n) => n.kind), ['round'], 'an id runner-state already lists is one node, not two');
assert.deepEqual(adNodes.filter((n) => /^X-a/.test(n.id)).map((n) => n.id), ['X-a-2'], 'a follow-up PR on a reused branch is one provisional node, from its title id');
assert.equal(adNodes.filter((n) => n.id === 'X-a-2').length, 1); assert.equal(ad.filter((r) => r.key === 'note' && r.label === 'Slice not in plan files yet' && r.value === 'X-a-2').length, 1, 'and one note');
assert.equal(adNodes.find((n) => n.id === 'X-placed-1').parent, 'ws-src', 'a placement rule decides the lane');
assert.ok(!adNodes.some((n) => n.id === 'X-done-1' || n.id === 'X-bare'), 'a merged X-* slice and a bare branch draw no provisional node');
// a merged X-* slice does not come back as a provisional node, and a bare branch does not make one
github.open = [pr(23, 'B0: Reopened', 'slice/B0', '', false)]; github.state = { updated_at: 'x', slots: { updated: new Date().toISOString(), items: [] } };
assert.ok(!(await toolR('plan_nodes', 'o/r10')).some((n) => n.kind === 'extra'), 'nothing provisional without an X-* id');
github.open = savedOpen; github.refs = savedRefs; github.closed = savedClosed;
github.state = stateBefore;

// free slots while work is ready: counted against the limit, and a slice whose `owns` overlap a running one is not offered
const ask = async (repo, slotSlices, prSlices = [], extra = {}) => {
  github.open = prSlices.map((id, n) => pr(60 + n, id + ': Running', 'slice/' + id, '', false));
  github.refs = (extra.branches || []).map((id) => ({ ref: 'refs/heads/slice/' + id }));
  const list = { items: extra.items || slotSlices.map((slice, n) => ({ slot: n + 1, slice, doing: 'busy' })) };
  if (!('updated' in extra)) list.updated = new Date().toISOString(); else if (extra.updated !== undefined) list.updated = extra.updated;
  github.state = { updated_at: 'x', slots: list };
  const m = await metaOf(repo), idle = m.find((r) => r.key === 'idle');
  return { idle, ids: idle ? idle.ids.split(',') : [], warn: m.filter((r) => r.key === 'warning').map((r) => r.label + ':' + r.value) };
};
// 1: one free slot (the boundary) with three running; overlaps by directory glob, mid-name star and brace set; a PR with no slot also blocks
let a1 = await ask('o/r11', ['B0', 'SRC-0', 'QQ-brace'], ['QQ-pronly']);
assert.ok(a1.idle && a1.idle.value.startsWith('1 free, '), 'one free slot is enough for the row');
for (const id of ['SRC-6', 'QQ-brace2', 'QQ-pclash']) assert.ok(!a1.ids.includes(id), id + ' overlaps something running (directory glob, brace set, a PR in no slot)');
for (const id of ['B0', 'SRC-0', 'QQ-brace', 'QQ-pronly']) assert.ok(!a1.ids.includes(id), id + ' is running, so it is not offered');
for (const id of ['SRC-13', 'SRC-7', 'QQ-held', 'SRC-9']) assert.ok(!a1.ids.includes(id), id + ' is waiting, held or for the owner, so it is not ready');
assert.ok(a1.ids.includes('SRC-8') && a1.ids.includes('QQ-md'), 'ready slices that overlap nothing are offered');
// 1b: wildcards inside a name: auth*.rs against auth_session.rs, StatusPill* against StatusPill.svelte (fresh slices, no branch or PR of their own)
let a1b = await ask('o/r11b', ['SRC-0', 'QQ-pill']);
assert.ok(!a1b.ids.includes('QQ-authsess'), 'auth*.rs overlaps auth_session.rs');
assert.ok(!a1b.ids.includes('QQ-pill2'), 'StatusPill* overlaps StatusPill.svelte');
assert.ok((await ask('o/r11c', [])).ids.includes('QQ-authsess'), 'and the same slices are offered when the star owner is not running');
// 1c: a slice that is active only through a slice/<id> branch (no slot, no PR) still blocks
let a1c = await ask('o/r11d', ['SRC-0'], [], { branches: ['QQ-branchonly'] });
assert.ok(!a1c.ids.includes('QQ-brclash') && !a1c.ids.includes('QQ-branchonly'), 'a branch-only slice counts as running');
assert.ok((await ask('o/r11e', ['SRC-0'])).ids.includes('QQ-brclash'), 'and without the branch it is offered');
// 2: a slot with no PR blocks, a slice that owns nothing is never blocked but is not offered while it runs; ? wildcard
let a2 = await ask('o/r12', ['QQ-slotonly', 'QQ-noowns', 'QQ-q']);
assert.ok(!a2.ids.includes('QQ-slotclash') && !a2.ids.includes('QQ-q2'), 'a slot with no PR and no branch still blocks its owns (directory and ?)');
assert.ok(!a2.ids.includes('QQ-noowns') && !a2.ids.includes('QQ-slotonly') && !a2.ids.includes('QQ-q'), 'a running slice is not offered even when it owns nothing');
// 3: bracket directories (SvelteKit routes) count as possible overlaps; a pattern that starts with a wildcard overlaps everything
let a3 = await ask('o/r13', ['QQ-br', 'QQ-md']);
assert.ok(!a3.ids.includes('QQ-br2') && !a3.ids.includes('QQ-star'), 'bracket directories and a leading wildcard are treated as possible overlaps');
// 4: nothing running offers a slice that owns nothing, and an explicit idle item is a free slot
let a4 = await ask('o/r14', [], [], { items: [{ slot: 1, status: 'idle', doing: 'kept but empty' }] });
assert.ok(a4.idle.value.startsWith('4 free') && a4.ids.includes('QQ-noowns'), 'an idle item is free, and owns: [] is offered');
assert.ok(a4.warn.includes('Bad owns:QQ-badowns'), 'owns that is not a list is reported');
// 5: every slot busy gives no row; a list that cannot be trusted gives no row
assert.equal((await ask('o/r15', ['B0', 'SRC-0', 'SRC-12', 'SRC-9'])).idle, undefined, 'no alarm when every slot is busy');
assert.equal((await ask('o/r16', [], [], { updated: '2020-01-01T00:00:00Z' })).idle, undefined, 'a stale list says nothing about free slots');
assert.equal((await ask('o/r17', [], [], { items: 'not a list' })).idle, undefined, 'a malformed list says nothing about free slots');
assert.equal((await ask('o/r18', [], [], { updated: undefined })).idle, undefined, 'a list with no date says nothing about free slots');
assert.equal((await ask('o/r19', [], [], { updated: 'yesterday-ish' })).idle, undefined, 'an unparseable date says nothing');
assert.equal((await ask('o/r20', [], [], { updated: '2999-01-01T00:00:00Z' })).idle, undefined, 'a future date says nothing');
github.state = null; // no runner-state at all: the plan files are still checked
assert.ok((await metaOf('o/r21')).some((r) => r.key === 'warning' && r.label === 'Bad owns' && r.value === 'QQ-badowns'), 'a non-list owns is caught even without runner-state, so CI sees it');
github.open = savedOpen; github.state = stateBefore;
github.fail = true; // a GitHub failure is reported to the caller, not hidden
const failed = (await (await worker.fetch(new Request('https://x/mcp/' + SECRET + '', { method: 'POST', body: JSON.stringify({ jsonrpc: '2.0', id: 5, method: 'tools/call', params: { name: 'plan_meta' } }) }), { ...env, CACHE_SECONDS: '0' })).json()).result;
assert.equal(failed.isError, true); assert.match(failed.content[0].text, /Could not read GitHub/);
console.log('status connector: all checks passed');
