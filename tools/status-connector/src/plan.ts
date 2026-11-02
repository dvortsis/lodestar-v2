// Turns what GitHub shows (the slice graph, pull requests, branches) into the dashboard's tables.
// Any agent leaves the same trace, so no agent needs to report anything.
import meta from './meta.json';

export interface PR { number: number; title: string; head_ref: string; merged_at?: string | null; body?: string | null; user?: string }
export interface Inputs {
  slices: any[];                 // runner/slices.yaml -> slices
  external: Record<string, { check?: string }>;
  runnerState: any | null;       // runner-state/state.json (optional; Claude runner only)
  mergedPRs: PR[];
  openPRs: PR[];
  branches: string[];            // ids from refs/heads/slice/<id>
  mainSha: string;
  mainDate: string;
  now: string;
}

// first match wins; patterns ending in * are prefixes
const RULES: [string, string[]][] = [
  ['ws-gates-found', ['G-v05', 'G-m5']],
  ['ws-settings', ['B2b', 'B2c', 'M-settings-form', 'S-settings-*', 'M-security-settings', 'B-log*', 'S-logs', 'S-about', 'B-update', 'S-updates', 'S-wizard', 'S-appearance', 'S-keys', 'M-auth']],
  ['ws-filters', ['M-filters-dialogs', 'S-filters', 'S-blocklists', 'B-subscriptions-api', 'B15', 'B17']],
  ['ws-search-ui', ['S-search-*', 'S-detail', 'S-compare', 'S-discover']],
  ['ws-insights', ['S-alerts', 'S-insights-*', 'B9', 'B11a', 'B11b', 'B11c']],
  ['ws-web', ['W*', 'C0', 'C1']],
  ['ws-ship', ['D-docker', 'R-docs', 'S-e2e', 'H-zh', 'S-doc-shots']],
  ['ws-api', ['/^B\\d/']],
  ['ws-land', ['/^L\\d$/']],
  ['ws-fix', ['F-*', 'X-ci-health', 'L4-followups', 'S-settings-reconcile']],
  ['ws-cut', ['CR-COL', 'CR-0d', 'CR-0f', 'D-v051', 'G-v051', 'D-v051-main', 'P-v052-cut']],
  ['ws-cr-build', ['CR-SIM', 'CR-gate-metrics', 'CR-ADM', 'CR-1a-H', 'CR-1a-S', 'CR-1b', 'CR-4', 'CR-1c']],
  ['ws-cr-live', ['CR-AB-*', 'CR-CP1']],
  ['ws-cr-tune', ['CR-5', 'CR-COMB', 'G-v06-crawler']],
  ['ws-src-core', ['SRC-0', 'SRC-1', 'SRC-2', 'SRC-3', 'SRC-4', 'SRC-5']],
  ['ws-src-parse', ['SRC-6', 'SRC-8', 'SRC-9']],
  ['ws-src-plug', ['SRC-7', 'SRC-10*', 'SRC-11*', 'SRC-12', 'SRC-13']],
  ['ws-src-ui', ['SRC-14', 'SRC-15*', 'SRC-16', 'SRC-17', 'SRC-G']],
  ['ws-src-plan', ['E-v06-sources', 'P-v06-rulepacks']],
  ['ws-imp-tools', ['IMP-*']],
  ['ws-imp-scale', ['X-search-files-3', 'G-import-*', 'CR-3']],
  ['ws-workers', ['E-v06-workers']],
  ['ws-companions', ['E-v06-companions']],
  ['ws-m6', ['E-M6', 'X-pack-static']],
  ['ws-m7-built', ['X-utp', 'X-integrations', 'X-clients', 'X-notify', 'X-email']],
  ['ws-m7-todo', ['E-M7', 'SB-6']],
  ['ws-v10', ['E-v08', 'E-M8']],
];
const hit = (id: string, p: string) => (p.startsWith('/') ? new RegExp(p.slice(1, -1)).test(id) : p.endsWith('*') ? id.startsWith(p.slice(0, -1)) : id === p);
export function place(id: string): string {
  if (new Set(meta.small).has(id)) return 'ws-small';
  for (const [ws, pats] of RULES) if (pats.some((p) => hit(id, p))) return ws;
  return 'ws-small';
}

const DONE = new Set(['merged', 'done_elsewhere', 'merged_elsewhere', 'passed', 'accepted', 'closed', 'merged-into-L5', 'ready-for-rc', 'done']);
const ACTIVE_PFX = ['building', 'review', 'ci', 'launched', 'changes', 'planning', 'pr-review'];
const STATES = ['done', 'closed', 'active', 'ready', 'waiting', 'you', 'held', 'planned', 'pending'];
const sliceIdOf = (pr: PR): string | null => {
  const m = /^([\w.-]+):/.exec(pr.title.trim()); if (m) return m[1];
  const b = /^slice\/(.+)$/.exec(pr.head_ref || ''); return b ? b[1] : null;
};
const agentOf = (pr: PR) => (/claude\.ai\/code|Generated with \[Claude Code\]/.test(pr.body || '') || /^claude\//.test(pr.head_ref || '') ? 'claude' : 'other');

export function buildPlan(inp: Inputs) {
  const SL = inp.slices, S = (inp.runnerState && inp.runnerState.slices) || {};
  const EXT: Record<string, { met: boolean; who: string; label: string }> = JSON.parse(JSON.stringify(meta.ext));
  for (const [k, v] of Object.entries(inp.external || {})) if (!EXT[k]) EXT[k] = { met: false, who: k.startsWith('owner-') ? 'you' : 'runner', label: (v.check || k).slice(0, 90) };
  const HELD = new Set(meta.held);
  const mergedById: Record<string, { pr: number; agent: string }> = {};
  const mergedNums = new Set<number>();
  for (const p of inp.mergedPRs) { mergedNums.add(p.number); const id = sliceIdOf(p); if (id) mergedById[id] = { pr: p.number, agent: agentOf(p) }; }
  const openById: Record<string, number> = {};
  for (const p of inp.openPRs) { const id = sliceIdOf(p); if (id) openById[id] = p.number; }
  const started = new Set(inp.branches);
  const ids = new Set<string>([...SL.map((s) => s.id), ...Object.keys(S)]);

  const cls = (i: string): string => {
    const v = S[i] || {}; const st = String(v.status ?? 'pending'); const pr = v.pr;
    if (mergedById[i]) return 'done';
    if (typeof pr === 'number' && mergedNums.has(pr)) return 'done';
    if (openById[i]) return 'active';
    if (st === 'superseded') return 'closed';
    if (DONE.has(st) || st.startsWith('merged')) return 'done';
    if (started.has(i)) return 'active';
    if (ACTIVE_PFX.some((p) => st.startsWith(p))) return 'active';
    if (st.startsWith('running-as')) return 'pending';
    if (st === 'held' || HELD.has(i) || st === 'deferred') return 'held';
    return 'pending';
  };
  const byid: Record<string, any> = {}; SL.forEach((s) => (byid[s.id] = s));
  const state: Record<string, string> = {}; SL.forEach((s) => (state[s.id] = cls(s.id)));
  state['E-v06-sources'] = 'expanded'; if (state['G-v051'] !== 'done') state['G-v051'] = 'active';
  HELD.forEach((i) => { if (state[i] && state[i] !== 'done') state[i] = 'held'; });
  const unmetExt = (s: any): string[] => (s.deps || []).filter((d: string) => d.startsWith('ext:')).map((d: string) => d.slice(4)).filter((k: string) => !(EXT[k] && EXT[k].met));
  const unmetDep = (s: any): string[] => (s.deps || []).filter((d: string) => !d.startsWith('ext:') && !['done', 'closed', 'expanded'].includes(state[d]));
  for (let it = 0; it < 6; it++) for (const s of SL) {
    const i = s.id; if (['done', 'closed', 'active', 'held', 'expanded'].includes(state[i])) continue;
    const ue = unmetExt(s), ud = unmetDep(s); const you = ue.some((e) => EXT[e] && EXT[e].who === 'you');
    if (!ue.length && !ud.length) state[i] = 'ready';
    else if (you && !ud.length) state[i] = 'you';
    else if (you && ud.every((d) => state[d] === 'you')) state[i] = 'you';
    else state[i] = 'waiting';
  }
  for (const s of SL) if (s.kind === 'epic' && state[s.id] === 'ready') state[s.id] = 'planned';

  const memo: Record<string, string[]> = {};
  const why = (i: string, seen: string[] = []): string[] => {
    if (memo[i]) return memo[i]; const s = byid[i]; let out: string[] = []; if (!s) return out;
    for (const e of unmetExt(s)) out.push('ext:' + e);
    for (const d of unmetDep(s)) { if (seen.includes(d)) continue; if (['active', 'ready', 'held', 'planned', 'you'].includes(state[d])) out.push(d); else out = out.concat(why(d, [...seen, i])); }
    return (memo[i] = [...new Set(out)]);
  };
  const kids: Record<string, string[]> = {};
  for (const s of SL) for (const d of s.deps || []) if (!d.startsWith('ext:')) (kids[d] = kids[d] || []).push(s.id);
  const down = (i: string, seen: Set<string>) => { for (const k of kids[i] || []) if (!seen.has(k)) { seen.add(k); down(k, seen); } return seen; };
  const first = (t: any) => String(t || '').replace(/\n/g, ' ').trim().slice(0, 240);

  const nodes: any[] = [];
  for (const p of meta.phases) nodes.push({ id: p.id, parent: 'root', level: 'phase', title: p.name, plain: p.plain, badge: p.badge, tag: p.tag });
  for (const [id, w] of Object.entries(meta.workstreams) as any) nodes.push({ id, parent: w.phase, level: 'ws', title: w.name, plain: w.plain });
  for (const s of SL) {
    const i = s.id, v = S[i] || {};
    const open = [...down(i, new Set())].filter((k) => !['done', 'closed', 'expanded'].includes(state[k]));
    const finished = ['done', 'closed'].includes(state[i]);
    nodes.push({
      id: i, parent: place(i), level: 'slice', title: s.title, agent: mergedById[i]?.agent || (v.session ? 'claude' : openById[i] ? 'other' : ''),
      kind: s.kind, model: s.model, state: state[i],
      pr: (typeof v.pr === 'number' ? v.pr : 0) || mergedById[i]?.pr || openById[i] || 0,
      deps: (s.deps || []).join(','), why: finished ? '' : why(i).join(';'), unlocks: finished ? 0 : open.length, note: first(s.notes),
    });
  }
  for (const [k, v] of Object.entries(S) as any) {
    if (ids.has(k) && byid[k]) continue; if (!v || typeof v !== 'object' || k.startsWith('runner-') || k === 'M4' || k === 'CR-0' || v.status == null) continue;
    let st = cls(k); if (st === 'held') st = 'done';
    nodes.push({ id: k, parent: 'ws-rounds', level: 'slice', title: first(v.notes || k).slice(0, 90) || k, agent: v.session ? 'claude' : '', kind: 'round', model: v.model || '', state: st, pr: typeof v.pr === 'number' ? v.pr : 0, deps: '', why: '', unlocks: 0, note: '' });
  }
  nodes.push({ id: 'root', parent: '', level: 'root', title: 'Lodestar 1.0', plain: 'A from-scratch DHT crawler and torrent search engine in one program for Docker, Unraid and Windows.' });
  const chil: Record<string, any[]> = {}; nodes.forEach((n) => (chil[n.parent] = chil[n.parent] || []).push(n));
  const roll = (n: any): Record<string, number> => {
    if (n.level === 'slice') return { [n.state]: 1 };
    const c: Record<string, number> = {};
    for (const k of chil[n.id] || []) { if (k.state === 'expanded') continue; if (k.id === 'ws-rounds' && n.level === 'phase') continue; const r = roll(k); for (const [a, b] of Object.entries(r)) c[a] = (c[a] || 0) + b; }
    for (const s of STATES) n[s] = c[s] || 0; n.total = Object.values(c).reduce((a, b) => a + b, 0); return c;
  };
  roll(nodes.find((n) => n.id === 'root'));
  for (const n of nodes) if (n.level !== 'slice' && n.total === undefined) roll(n);
  for (const n of nodes) {
    if (n.level === 'slice') { for (const s of STATES) n[s] = n.state === s ? 1 : 0; n.total = 1; }
    for (const k of ['agent', 'kind', 'model', 'state', 'pr', 'deps', 'why', 'unlocks', 'note', 'badge', 'tag', 'plain']) if (n[k] === undefined) n[k] = k === 'pr' || k === 'unlocks' ? 0 : '';
    if (n.level !== 'slice') n.state = n.total && n.done + n.closed === n.total ? 'done' : n.active ? 'active' : n.ready || n.planned ? 'ready' : n.you ? 'you' : n.held ? 'held' : 'waiting';
  }
  const road = meta.road.map((r: any) => {
    const c: Record<string, number> = {}; r.members.filter((m: string) => state[m]).forEach((m: string) => (c[state[m]] = (c[state[m]] || 0) + 1));
    const tot = Object.values(c).reduce((a, b) => a + b, 0); const done = (c.done || 0) + (c.closed || 0);
    const st = r.override || (tot && done === tot ? 'done' : c.active ? 'active' : c.you ? 'you' : c.held && !c.ready ? 'held' : c.ready || c.planned ? 'ready' : 'waiting');
    return { id: r.id, label: r.label, plain: r.plain, col: r.col, row: r.row, state: st, done, total: tot, members: r.members.join(',') };
  });
  const byDay: Record<string, number> = {}; const merged = [...inp.mergedPRs].filter((p) => p.merged_at).sort((a, b) => (a.merged_at! < b.merged_at! ? 1 : -1));
  for (const p of merged) { const d = p.merged_at!.slice(0, 10); byDay[d] = (byDay[d] || 0) + 1; }
  const activity = Object.keys(byDay).sort().slice(-14).map((d) => ({ day: d, merged: byDay[d] }));
  const recent = merged.slice(0, 14).map((p) => ({ pr: p.number, sha: '', when: p.merged_at!.slice(0, 16).replace('T', ' ') + 'Z', title: p.title.slice(0, 110), agent: agentOf(p) }));
  const metaRows = [
    { key: 'main', label: 'Main branch', value: inp.mainSha.slice(0, 8), note: inp.mainDate },
    { key: 'state', label: 'Runner state last pushed', value: String(inp.runnerState?.updated_at || 'n/a'), note: 'runner-state/state.json (Claude runner only; optional)' },
    { key: 'graph', label: 'Slice graph', value: 'main', note: 'runner/slices.yaml' },
    { key: 'built', label: 'Built at', value: inp.now.slice(0, 16).replace('T', ' ') + ' UTC', note: 'live, by the status connector' },
  ];
  return { nodes, road, activity, recent, meta: metaRows };
}
