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

let cache: { at: number; plan: any } | null = null;

async function gh(env: Env, path: string, raw = false): Promise<any> {
  const r = await fetch('https://api.github.com' + path, {
    headers: {
      Authorization: 'Bearer ' + env.GITHUB_TOKEN, 'User-Agent': 'lodestar-status-connector', 'X-GitHub-Api-Version': '2022-11-28',
      Accept: raw ? 'application/vnd.github.raw+json' : 'application/vnd.github+json',
    },
  });
  if (!r.ok) throw new Error(`GitHub ${r.status} for ${path}`);
  return raw ? r.text() : r.json();
}

const toPR = (p: any): PR => ({ number: p.number, title: p.title, head_ref: p.head?.ref || '', merged_at: p.merged_at, body: p.body, user: p.user?.login });

async function load(env: Env) {
  if (cache && Date.now() - cache.at < Number(env.CACHE_SECONDS ?? 60) * 1000) return cache.plan;
  const repo = env.GITHUB_REPO;
  const closedPages = [1, 2, 3, 4].map((n) => gh(env, `/repos/${repo}/pulls?state=closed&base=main&sort=updated&direction=desc&per_page=100&page=${n}`));
  const [yamlText, state, open, branches, main, ...closed] = await Promise.all([
    gh(env, `/repos/${repo}/contents/runner/slices.yaml?ref=main`, true),
    gh(env, `/repos/${repo}/contents/state.json?ref=runner-state`, true).then((t) => JSON.parse(t)).catch(() => null),
    gh(env, `/repos/${repo}/pulls?state=open&per_page=100`),
    gh(env, `/repos/${repo}/git/matching-refs/heads/slice/`).catch(() => []),
    gh(env, `/repos/${repo}/commits/main`),
    ...closedPages,
  ]);
  const y = parse(yamlText);
  const plan = buildPlan({
    slices: y.slices, external: y.external || {}, runnerState: state,
    mergedPRs: closed.flat().filter((p: any) => p.merged_at).map(toPR), openPRs: open.map(toPR),
    branches: (branches as any[]).map((b) => String(b.ref).replace('refs/heads/slice/', '')),
    mainSha: main.sha, mainDate: main.commit?.committer?.date || '', now: new Date().toISOString(),
  });
  // keep answers small: leave out empty fields; the dashboard puts them back
  plan.nodes = plan.nodes.map((n: any) => Object.fromEntries(Object.entries(n).filter(([k, v]) => ['id', 'parent', 'level', 'state', 'title', 'total'].includes(k) || (v !== '' && v !== 0 && v !== undefined))));
  cache = { at: Date.now(), plan };
  return plan;
}

const rpc = (id: any, result: any) => new Response(JSON.stringify({ jsonrpc: '2.0', id, result }), { headers: { 'content-type': 'application/json' } });
const rpcErr = (id: any, code: number, message: string) => new Response(JSON.stringify({ jsonrpc: '2.0', id, error: { code, message } }), { headers: { 'content-type': 'application/json' } });

export default {
  async fetch(req: Request, env: Env): Promise<Response> {
    const url = new URL(req.url);
    if (url.pathname !== '/mcp/' + env.PATH_SECRET) return new Response('Not found', { status: 404 });
    if (req.method === 'GET' || req.method === 'DELETE') return new Response('Method not allowed', { status: 405, headers: { Allow: 'POST' } });
    if (req.method !== 'POST') return new Response('Method not allowed', { status: 405 });
    let msg: any; try { msg = await req.json(); } catch { return rpcErr(null, -32700, 'Parse error'); }
    if (Array.isArray(msg)) return rpcErr(null, -32600, 'Batches are not supported');
    const { id, method, params } = msg;
    if (id === undefined) return new Response(null, { status: 202 }); // notification
    switch (method) {
      case 'initialize':
        return rpc(id, { protocolVersion: params?.protocolVersion || '2025-06-18', capabilities: { tools: {} }, serverInfo: { name: 'lodestar-status', version: '1.0.0' } });
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
