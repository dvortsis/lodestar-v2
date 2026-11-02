// Lodestar status connector: a read-only MCP server (Streamable HTTP, stateless) for a Cloudflare Worker.
// It reads GitHub and returns the plan's tables. It writes nothing anywhere.
import { parse } from 'yaml';
import { buildPlan, type PR } from './plan';

interface Env { GITHUB_TOKEN: string; GITHUB_REPO: string; PATH_SECRET: string; CACHE_SECONDS?: string }

const TOOLS = [
  ['plan_nodes', 'Every phase, workstream and slice of the Lodestar plan with its state, derived from GitHub. Fields that are empty or zero are left out.'],
  ['plan_road', 'The big roadmap steps with their state rolled up from their slices.'],
  ['plan_activity', 'Pull requests merged into main per UTC day, last 14 days with merges.'],
  ['plan_recent', 'The 14 most recently merged pull requests, with who made them (claude or other).'],
  ['plan_meta', 'Which commit and runner-state push the tables were built from.'],
] as const;
const PROTOCOLS = ['2025-06-18', '2025-03-26', '2024-11-05'];
const MIN_SECRET = 32;
const MAX_CLOSED_PAGES = 20;

// ---- access: the secret in the path is the only password, so it must be set, long, and compared in constant time
const digest = async (s: string) => new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(s)));
async function pathAllowed(pathname: string, secret: string | undefined): Promise<boolean> {
  if (!secret || secret.length < MIN_SECRET) return false; // unset, empty or short: nothing is served
  const [a, b] = await Promise.all([digest(pathname), digest('/mcp/' + secret)]);
  let diff = 0; for (let i = 0; i < a.length; i++) diff |= a[i] ^ b[i];
  return diff === 0;
}

// ---- GitHub, with a cache shared by every isolate in the data centre, one fetch for concurrent misses, and short error caching
const inflight = new Map<string, Promise<any>>();
const failures = new Map<string, { at: number; message: string }>();
const ERROR_SECONDS = 30;
const cacheStore = (): Cache | null => (typeof caches !== 'undefined' && (caches as any).default ? (caches as any).default : null);

async function ghFetch(env: Env, path: string, raw: boolean): Promise<any> {
  const r = await fetch('https://api.github.com' + path, {
    headers: {
      Authorization: 'Bearer ' + env.GITHUB_TOKEN, 'User-Agent': 'lodestar-status-connector', 'X-GitHub-Api-Version': '2022-11-28',
      Accept: raw ? 'application/vnd.github.raw+json' : 'application/vnd.github+json',
    },
  });
  if (!r.ok) throw new Error(`GitHub ${r.status} for ${path}`);
  return raw ? r.text() : r.json();
}

async function gh(env: Env, path: string, ttl: number, raw = false): Promise<any> {
  const key = (raw ? 'raw:' : 'json:') + path;
  const bad = failures.get(key);
  if (bad && Date.now() - bad.at < ERROR_SECONDS * 1000) throw new Error(bad.message);
  const store = cacheStore(); const req = new Request('https://cache.lodestar-status.invalid/' + encodeURIComponent(env.GITHUB_REPO + key));
  if (ttl > 0 && store) { const hit = await store.match(req); if (hit) return raw ? hit.text() : hit.json(); }
  let p = inflight.get(key);
  if (!p) {
    p = ghFetch(env, path, raw).then(async (v) => {
      failures.delete(key);
      if (ttl > 0 && store) await store.put(req, new Response(raw ? v : JSON.stringify(v), { headers: { 'Cache-Control': `max-age=${ttl}` } }));
      return v;
    }, (e) => { failures.set(key, { at: Date.now(), message: String(e?.message || e) }); throw e; }).finally(() => inflight.delete(key));
    inflight.set(key, p);
  }
  return p;
}

const toPR = (p: any): PR => ({ number: p.number, title: p.title, head_ref: p.head?.ref || '', merged_at: p.merged_at, body: p.body, user: p.user?.login });

async function closedPRs(env: Env, ttl: number): Promise<any[]> {
  // newest first; keep going until a page is not full, so no merged slice drops out as the repository grows.
  // Older pages change rarely, so they are cached ten times longer than the first.
  const all: any[] = [];
  for (let start = 1; start <= MAX_CLOSED_PAGES; start += 4) {
    const pages = await Promise.all([0, 1, 2, 3].filter((k) => start + k <= MAX_CLOSED_PAGES).map((k) => gh(env, `/repos/${env.GITHUB_REPO}/pulls?state=closed&base=main&sort=updated&direction=desc&per_page=100&page=${start + k}`, start + k === 1 ? ttl : ttl * 10)));
    for (const pg of pages) all.push(...pg);
    if (pages[pages.length - 1].length < 100 || pages.some((pg) => pg.length < 100)) break;
  }
  return all;
}

let memo: { at: number; plan: any } | null = null;
async function load(env: Env) {
  const ttl = Number(env.CACHE_SECONDS ?? 60);
  if (memo && Date.now() - memo.at < Math.min(ttl, 10) * 1000) return memo.plan;
  const repo = env.GITHUB_REPO;
  const [yamlText, state, open, branches, main, closed] = await Promise.all([
    gh(env, `/repos/${repo}/contents/runner/slices.yaml?ref=main`, ttl, true),
    gh(env, `/repos/${repo}/contents/state.json?ref=runner-state`, ttl, true).then((t) => JSON.parse(t)).catch(() => null),
    gh(env, `/repos/${repo}/pulls?state=open&per_page=100`, ttl),
    gh(env, `/repos/${repo}/git/matching-refs/heads/slice/`, ttl).catch(() => []),
    gh(env, `/repos/${repo}/commits/main`, ttl),
    closedPRs(env, ttl),
  ]);
  const y = parse(yamlText);
  const plan = buildPlan({
    slices: y.slices, external: y.external || {}, runnerState: state,
    mergedPRs: closed.filter((p: any) => p.merged_at).map(toPR), closedPRs: closed.filter((p: any) => !p.merged_at).map(toPR), openPRs: open.map(toPR),
    branches: (branches as any[]).map((b) => String(b.ref).replace('refs/heads/slice/', '')),
    mainSha: main.sha, mainDate: main.commit?.committer?.date || '', now: new Date().toISOString(),
  });
  // keep answers small: leave out empty fields; the dashboard puts them back
  plan.nodes = plan.nodes.map((n: any) => Object.fromEntries(Object.entries(n).filter(([k, v]) => ['id', 'parent', 'level', 'state', 'title', 'total'].includes(k) || (v !== '' && v !== 0 && v !== undefined))));
  memo = { at: Date.now(), plan };
  return plan;
}

const rpc = (id: any, result: any) => new Response(JSON.stringify({ jsonrpc: '2.0', id, result }), { headers: { 'content-type': 'application/json' } });
const rpcErr = (id: any, code: number, message: string) => new Response(JSON.stringify({ jsonrpc: '2.0', id, error: { code, message } }), { headers: { 'content-type': 'application/json' } });

export default {
  async fetch(req: Request, env: Env): Promise<Response> {
    const url = new URL(req.url);
    if (!(await pathAllowed(url.pathname, env.PATH_SECRET))) return new Response('Not found', { status: 404 });
    if (req.method !== 'POST') return new Response('Method not allowed', { status: 405, headers: { Allow: 'POST' } });
    let msg: any; try { msg = await req.json(); } catch { return rpcErr(null, -32700, 'Parse error'); }
    if (typeof msg !== 'object' || msg === null || Array.isArray(msg)) return rpcErr(null, -32600, 'Invalid request: send one JSON-RPC object');
    const { id, method, params } = msg;
    if (id === undefined) return new Response(null, { status: 202 }); // notification
    switch (method) {
      case 'initialize':
        return rpc(id, { protocolVersion: PROTOCOLS.includes(params?.protocolVersion) ? params.protocolVersion : PROTOCOLS[0], capabilities: { tools: {} }, serverInfo: { name: 'lodestar-status', version: '1.1.0' } });
      case 'ping': return rpc(id, {});
      case 'tools/list':
        return rpc(id, { tools: TOOLS.map(([name, description]) => ({ name, description, inputSchema: { type: 'object', properties: {}, additionalProperties: false }, annotations: { readOnlyHint: true, openWorldHint: true } })) });
      case 'tools/call': {
        const name = params?.name as string; const tool = TOOLS.find((t) => t[0] === name);
        if (!tool) return rpcErr(id, -32602, 'Unknown tool ' + name);
        try {
          const plan = await load(env); const rows = plan[name.replace('plan_', '')];
          return rpc(id, { content: [{ type: 'text', text: JSON.stringify(rows) }], isError: false });
        } catch (e: any) { return rpc(id, { content: [{ type: 'text', text: 'Could not read GitHub: ' + (e?.message || e) }], isError: true }); }
      }
      default: return rpcErr(id, -32601, 'Method not found: ' + method);
    }
  },
};
