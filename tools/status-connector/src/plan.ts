// Turns what GitHub shows (the slice graph, pull requests, branches) into the dashboard's tables.
// Any agent leaves the same trace, so no agent needs to report anything.
export interface PR { number: number; title: string; head_ref: string; merged_at?: string | null; body?: string | null; user?: string }
export interface Inputs {
  slices: any[];                 // runner/slices.yaml -> slices
  external: Record<string, { check?: string }>;
  cfg: any;                      // runner/dashboard.yaml
  runnerState: any | null;       // runner-state/state.json (optional; Claude runner only)
  mergedPRs: PR[];
  closedPRs?: PR[];              // closed without merging
  openPRs: PR[];
  branches: string[];            // ids from refs/heads/slice/<id>
  refs?: { branches: string[]; tags: string[] }; // names GitHub knows, for conditions with `auto:`
  mainSha: string;
  mainDate: string;
  now: string;
}

const hit = (id: string, p: string) => (p.startsWith('/') ? new RegExp(p.slice(1, -1)).test(id) : p.endsWith('*') ? id.startsWith(p.slice(0, -1)) : id === p);
export function place(cfg: any, id: string): { ws: string; placed: boolean } {
  const pin = cfg.pinned?.[id]; if (pin) return { ws: pin, placed: true };
  for (const r of cfg.placement || []) if ((r.match || []).some((p: string) => hit(id, p))) return { ws: r.ws, placed: true };
  return { ws: cfg.unplaced_workstream || 'ws-small', placed: false };
}

const DONE = new Set(['merged', 'done_elsewhere', 'merged_elsewhere', 'passed', 'accepted', 'closed', 'merged-into-L5', 'ready-for-rc', 'done']);
const ACTIVE_PFX = ['building', 'review', 'ci', 'launched', 'changes', 'planning', 'pr-review'];
const STATES = ['done', 'closed', 'active', 'ready', 'waiting', 'you', 'held', 'planned', 'pending'];
// "SRC-4: x", "SRC-4/SRC-5: x" and "SRC-4, SRC-5: x" all name slices; so does a head branch slice/<id>.
const sliceIdsOf = (pr: PR): string[] => {
  const m = /^([\w.,\/+ -]+?):/.exec(pr.title.trim());
  if (m) { const ids = m[1].split(/[\/,+\s]+/).filter((x) => /^[\w.-]+$/.test(x)); if (ids.length) return ids; }
  const b = /^slice\/(.+)$/.exec(pr.head_ref || ''); return b ? [b[1]] : [];
};
const agentOf = (pr: PR) => (/claude\.ai\/code|Generated with \[Claude Code\]/.test(pr.body || '') || /^claude\//.test(pr.head_ref || '') ? 'claude' : 'other');

export function buildPlan(inp: Inputs) {
  const cfg = inp.cfg, SL = inp.slices, S = (inp.runnerState && inp.runnerState.slices) || {};
  const warnings: { label: string; value: string; note: string }[] = [];
  const warn = (label: string, value: string, note: string) => warnings.push({ label, value, note });

  // outside conditions: the file's, then GitHub's answer for `auto:`; ones slices.yaml names but the file doesn't are unmet
  const EXT: Record<string, { met: boolean; who: string; label: string }> = {};
  for (const [k, v] of Object.entries(cfg.conditions || {}) as any) {
    let met = !!v.met;
    if (v.auto && inp.refs) { const [kind, name] = String(v.auto).split(/:(.*)/s); met = kind === 'tag' ? inp.refs.tags.includes(name) : kind === 'branch' ? inp.refs.branches.includes(name) : met; }
    EXT[k] = { met, who: v.who || 'runner', label: v.label || k };
  }
  const used = new Set<string>([...Object.keys(inp.external || {}), ...SL.flatMap((s) => (s.deps || []).filter((d: string) => d.startsWith('ext:')).map((d: string) => d.slice(4)))]);
  for (const k of used) if (!EXT[k]) {
    EXT[k] = { met: false, who: k.startsWith('owner-') ? 'you' : 'runner', label: ((inp.external || {})[k]?.check || k).slice(0, 90) };
    warn('Condition not described', k, 'runner/slices.yaml uses ext:' + k + ' but runner/dashboard.yaml has no conditions entry for it');
  }
  const HELD = new Set<string>(cfg.held || []), EXPANDED = new Set<string>(cfg.expanded || []);
  const mergedById: Record<string, { pr: number; agent: string }> = {};
  const mergedNums = new Set<number>();
  for (const p of inp.mergedPRs) { mergedNums.add(p.number); for (const id of sliceIdsOf(p)) mergedById[id] = { pr: p.number, agent: agentOf(p) }; }
  const openById: Record<string, number> = {};
  for (const p of inp.openPRs) for (const id of sliceIdsOf(p)) openById[id] = p.number;
  // a branch that only has closed, unmerged PRs is abandoned work, not work in progress
  const abandoned = new Set<string>(); for (const p of inp.closedPRs || []) for (const id of sliceIdsOf(p)) abandoned.add(id);
  const started = new Set(inp.branches.filter((b) => !abandoned.has(b)));

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
    if (st === 'held' || st === 'deferred') return 'held';
    return 'pending';
  };
  const byid: Record<string, any> = {}; SL.forEach((s) => (byid[s.id] = s));
  const state: Record<string, string> = {}; SL.forEach((s) => (state[s.id] = cls(s.id)));
  for (const id of EXPANDED) { if (state[id]) state[id] = 'expanded'; else warn('Unknown slice', id, 'dashboard.yaml expanded: names a slice that is not in runner/slices.yaml'); }
  for (const [id, st] of Object.entries(cfg.state_override || {}) as any) { if (!state[id]) warn('Unknown slice', id, 'dashboard.yaml state_override: names a slice that is not in runner/slices.yaml'); else if (state[id] !== 'done') state[id] = st; }
  HELD.forEach((i) => { if (!state[i]) warn('Unknown slice', i, 'dashboard.yaml held: names a slice that is not in runner/slices.yaml'); else if (state[i] !== 'done' && state[i] !== 'expanded') state[i] = 'held'; });
  const FIXED = new Set(Object.keys(cfg.state_override || {}));
  const unmetExt = (s: any): string[] => (s.deps || []).filter((d: string) => d.startsWith('ext:')).map((d: string) => d.slice(4)).filter((k: string) => !(EXT[k] && EXT[k].met));
  const unmetDep = (s: any): string[] => (s.deps || []).filter((d: string) => !d.startsWith('ext:') && !['done', 'closed', 'expanded'].includes(state[d]));
  for (let it = 0; it < 6; it++) for (const s of SL) {
    const i = s.id; if (['done', 'closed', 'active', 'held', 'expanded'].includes(state[i]) || FIXED.has(i)) continue;
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

  const wsIds = new Set(Object.keys(cfg.workstreams || {}));
  const nodes: any[] = [];
  for (const p of cfg.phases || []) nodes.push({ id: p.id, parent: 'root', level: 'phase', title: p.name, plain: p.plain, badge: p.badge, tag: p.tag });
  for (const [id, w] of Object.entries(cfg.workstreams || {}) as any) nodes.push({ id, parent: w.phase, level: 'ws', title: w.name, plain: w.plain });
  const unplaced: string[] = [];
  for (const s of SL) {
    const i = s.id, v = S[i] || {};
    const pl = place(cfg, i); if (!pl.placed) unplaced.push(i);
    if (!wsIds.has(pl.ws)) warn('Unknown workstream', pl.ws, 'placement for ' + i + ' names a workstream that dashboard.yaml does not define');
    const open = [...down(i, new Set())].filter((k) => !['done', 'closed', 'expanded'].includes(state[k]));
    const finished = ['done', 'closed'].includes(state[i]);
    nodes.push({
      id: i, parent: pl.ws, level: 'slice', title: s.title, agent: mergedById[i]?.agent || (v.session ? 'claude' : openById[i] ? 'other' : ''),
      kind: s.kind, model: s.model, state: state[i],
      pr: (typeof v.pr === 'number' ? v.pr : 0) || mergedById[i]?.pr || openById[i] || 0,
      deps: (s.deps || []).join(','), why: finished ? '' : why(i).join(';'), unlocks: finished ? 0 : open.length, note: first(s.notes),
    });
  }
  for (const [k, v] of Object.entries(S) as any) {
    if (byid[k]) continue; if (!v || typeof v !== 'object' || k.startsWith('runner-') || k === 'M4' || k === 'CR-0' || v.status == null) continue;
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
  const steps = cfg.roadmap?.steps || [];
  const road = steps.map((r: any) => {
    for (const m of r.members || []) if (!state[m]) warn('Unknown slice', m, 'roadmap step "' + r.id + '" lists a slice that is not in runner/slices.yaml');
    const mem: string[] = (r.members || []).filter((m: string) => state[m]);
    const c: Record<string, number> = {}; mem.forEach((m) => (c[state[m]] = (c[state[m]] || 0) + 1));
    const tot = Object.values(c).reduce((a, b) => a + b, 0); const done = (c.done || 0) + (c.closed || 0);
    const st = r.override || (tot && done === tot ? 'done' : c.active ? 'active' : c.you ? 'you' : c.held && !c.ready ? 'held' : c.ready || c.planned ? 'ready' : 'waiting');
    return { id: r.id, label: r.label, plain: r.plain, col: r.col, row: r.row, state: st, done, total: tot, members: mem.join(',') };
  });
  const stepIds = new Set(steps.map((r: any) => r.id));
  const edges = (cfg.roadmap?.arrows || []).filter(([a, b]: string[]) => { const ok = stepIds.has(a) && stepIds.has(b); if (!ok) warn('Unknown roadmap step', a + ' -> ' + b, 'an arrow in dashboard.yaml names a step that is not defined'); return ok; }).map(([a, b]: string[]) => ({ source: a, target: b }));

  const byDay: Record<string, number> = {}; const merged = [...inp.mergedPRs].filter((p) => p.merged_at).sort((a, b) => (a.merged_at! < b.merged_at! ? 1 : -1));
  for (const p of merged) { const d = p.merged_at!.slice(0, 10); byDay[d] = (byDay[d] || 0) + 1; }
  const activity = Object.keys(byDay).sort().slice(-14).map((d) => ({ day: d, merged: byDay[d] }));
  const recent = merged.slice(0, 14).map((p) => ({ pr: p.number, sha: '', when: p.merged_at!.slice(0, 16).replace('T', ' ') + 'Z', title: p.title.slice(0, 110), agent: agentOf(p) }));

  const gates = (cfg.gates || []).map((g: any) => {
    let status = g.status || 'not started';
    if (g.slice) { if (!state[g.slice]) warn('Unknown slice', g.slice, 'gate "' + g.id + '" names a slice that is not in runner/slices.yaml'); else status = ['done', 'closed'].includes(state[g.slice]) ? 'passed' : 'open'; }
    return { id: g.id, name: g.name, plain: g.plain || '', status, measure: g.measure || '', target: g.target || '', where: g.where || '' };
  });
  const owner = (cfg.owner_queue || []).map((o: any) => ({ n: o.n, type: o.type, action: o.action, unblocks: o.unblocks, when: o.when }));
  const ext = Object.entries(EXT).map(([key, v]) => ({ key, met: v.met, who: v.who, label: v.label }));
  const asof = String(cfg.notes?.updated || '');
  const notes: any[] = [];
  for (const g of ['now', 'you']) (cfg.notes?.[g] || []).forEach((x: any, i: number) => notes.push({ group: g, order: i + 1, label: x.label, text: x.text, asof }));
  const crawler = (cfg.crawler || []).map((c: any) => ({ label: c.label, value: c.value, kind: c.kind, note: c.note || '' }));
  const soak = (cfg.soak || []).map((x: any) => ({ stage: x.stage, label: x.label, start: x.start, end: x.end }));

  if (unplaced.length) warn('Slices not placed', String(unplaced.length), 'No placement rule covers: ' + unplaced.slice(0, 12).join(', ') + (unplaced.length > 12 ? ', …' : '') + '. Add them to placement: in runner/dashboard.yaml.');
  const metaRows: any[] = [
    { key: 'main', label: 'Main branch', value: inp.mainSha.slice(0, 8), note: inp.mainDate },
    { key: 'state', label: 'Runner state last pushed', value: String(inp.runnerState?.updated_at || 'n/a'), note: 'runner-state/state.json (Claude runner only; optional)' },
    { key: 'graph', label: 'Plan files', value: 'main', note: 'runner/slices.yaml and runner/dashboard.yaml' },
    { key: 'built', label: 'Built at', value: inp.now.slice(0, 16).replace('T', ' ') + ' UTC', note: 'live, by the status connector' },
    ...warnings.map((w) => ({ key: 'warning', label: w.label, value: w.value, note: w.note })),
  ];
  return { nodes, road, edges, activity, recent, meta: metaRows, gates, owner, ext, notes, crawler, soak };
}
