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
  limits?: { phase_total?: number };  // runner/slices.yaml -> limits
  historyIncomplete?: boolean;   // GitHub did not answer for some older pages of closed pull requests
  openPRs: PR[];
  branches: string[];            // ids from refs/heads/slice/<id>
  refs?: { branches: string[]; tags: string[] }; // names GitHub knows, for conditions with `auto:`
  mainSha: string;
  mainDate: string;
  now: string;
}

// a /regex/ that does not compile matches nothing (it is reported by buildPlan, never thrown on every refresh)
const hit = (id: string, p: string) => {
  if (p.startsWith('/')) { try { return new RegExp(p.slice(1, -1)).test(id); } catch { return false; } }
  return p.endsWith('*') ? id.startsWith(p.slice(0, -1)) : id === p;
};
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
// an open pull request also counts for the slice its branch is named after: a follow-up on a reused branch keeps its title id
const openIdsOf = (pr: PR): string[] => { const b = /^slice\/(.+)$/.exec(pr.head_ref || ''); return [...new Set([...sliceIdsOf(pr), ...(b ? [b[1]] : [])])]; };
const stripId = (title: string) => title.replace(/^[^:]*:\s*/, '').slice(0, 120);
const agentOf = (pr: PR) => (/claude\.ai\/code|Generated with \[Claude Code\]/.test(pr.body || '') || /^claude\//.test(pr.head_ref || '') ? 'claude' : 'other');

export function buildPlan(inp: Inputs) {
  const cfg = inp.cfg, SL = inp.slices, S = (inp.runnerState && inp.runnerState.slices) || {};
  const warnings: { label: string; value: string; note: string }[] = [];
  const warn = (label: string, value: string, note: string) => warnings.push({ label, value, note });
  // a note is a quieter warning: something that is fine for now but should be written down in the plan files; the page lists it apart
  const notices: { label: string; value: string; note: string }[] = [];
  const notice = (label: string, value: string, note: string) => notices.push({ label, value, note });

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
  for (const k of Object.keys(EXT)) if (!used.has(k)) warn('Condition unused', k, 'runner/dashboard.yaml describes ext:' + k + ' but runner/slices.yaml neither lists nor uses it; remove it or add it to the external section');
  for (const r of cfg.placement || []) for (const p of r.match || []) if (p.startsWith('/')) { try { new RegExp(p.slice(1, -1)); } catch (e: any) { warn('Bad placement pattern', p, 'for ' + r.ws + ': ' + (e?.message || e) + '. It matches nothing until fixed'); } }
  const HELD = new Set<string>(cfg.held || []), EXPANDED = new Set<string>(cfg.expanded || []);
  const mergedById: Record<string, { pr: number; agent: string }> = {};
  const mergedNums = new Set<number>();
  for (const p of inp.mergedPRs) { mergedNums.add(p.number); for (const id of sliceIdsOf(p)) mergedById[id] = { pr: p.number, agent: agentOf(p) }; }
  const openById: Record<string, number> = {}, openPR = new Map<string, PR>(); // a Map: a slot slice named `constructor` or `__proto__` must not find anything
  for (const p of inp.openPRs) for (const id of openIdsOf(p)) { openById[id] = p.number; openPR.set(id, p); }
  // a branch that only has closed, unmerged PRs is abandoned work, not work in progress
  const abandoned = new Set<string>(); for (const p of inp.closedPRs || []) for (const id of openIdsOf(p)) abandoned.add(id);
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
  for (const id of Object.keys(cfg.pinned || {})) if (!byid[id]) warn('Unknown slice', id, 'dashboard.yaml pinned: names a slice that is not in runner/slices.yaml');
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
  // an X-* slice that has an open pull request but is not in slices.yaml yet is shown at once in its placement (or the default lane).
  // The title id decides (a PR titled X-a-2 on slice/X-a is one slice, not two), and an id runner-state already lists is drawn as its round below.
  const isRound = (k: string, v: any) => !!v && typeof v === 'object' && !k.startsWith('runner-') && k !== 'M4' && k !== 'CR-0' && v.status != null;
  const provisional = new Set<string>();
  for (const p of inp.openPRs) for (const id of sliceIdsOf(p)) {
    if (provisional.has(id) || byid[id] || mergedById[id] || isRound(id, S[id]) || !/^X-[\w.-]+$/.test(id)) continue;
    provisional.add(id);
    const pl = place(cfg, id), ws = wsIds.has(pl.ws) ? pl.ws : cfg.unplaced_workstream || 'ws-small';
    nodes.push({ id, parent: ws, level: 'slice', title: stripId(p.title) || id, agent: agentOf(p), kind: 'extra', model: '', state: 'active', pr: p.number, deps: '', why: '', unlocks: 0, note: 'Not in runner/slices.yaml yet' });
    notice('Slice not in plan files yet', id, 'PR #' + p.number + ' is for ' + id + ', which runner/slices.yaml does not list yet; it is shown as active until it does');
  }
  for (const [k, v] of Object.entries(S) as any) {
    if (byid[k] || !isRound(k, v)) continue;
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

  // worker slots: the runner records who works on what in runner-state's state.json (`slots`), pushed on the wake where a session
  // starts, finishes, is archived or changes task, with no PR. GitHub supplies each slice's title, state and PR. The number of slots
  // is the owner's limit in runner/slices.yaml (limits.phase_total), not a second copy. Anything out of step is reported, not hidden.
  const slots: any[] = [], idle: { label: string; value: string; note: string }[] = [];
  const sl = inp.runnerState && inp.runnerState.slots ? inp.runnerState.slots : null;
  const text = (v: any): string => (typeof v === 'string' ? v : typeof v === 'number' || typeof v === 'boolean' ? String(v) : '').slice(0, 300);
  const MAX_SLOTS = 64;
  const ROLES = ['build', 'review', 'docs', 'soak', 'other'], AGENTS = ['claude', 'other'];
  if (sl) {
    if (!Array.isArray(sl.items)) warn('Slots malformed', typeof sl.items, 'runner-state slots.items must be a list of entries; it is not, so no slot is shown');
    const items: any[] = Array.isArray(sl.items) ? sl.items : [];
    // rows are drawn for 1..limit only; a typo like slot 1000000000 must not cost memory or break the other tables
    const limit = Number(inp.limits?.phase_total);
    const cap = Number.isInteger(limit) && limit > 0 ? Math.min(limit, MAX_SLOTS) : 0;
    if (!cap) warn('Slot limit unknown', String(inp.limits?.phase_total ?? 'none'), 'limits.phase_total in runner/slices.yaml is missing or not a whole number above 0, so no slot rows are drawn');
    const byId: Record<string, any> = {}; for (const n of nodes) byId[n.id] = n;
    const slotAsof = typeof sl.updated === 'string' ? sl.updated : '';
    const seenSlice = new Map<string, number>(), seenSlot = new Set<number>();
    const rows: Record<number, any> = {};
    for (const x of items) {
      if (!x || typeof x !== 'object' || !Number.isInteger(x.slot) || x.slot < 1) { warn('Bad slot number', String(x?.slot).slice(0, 20), 'a slots entry in runner-state must have a whole slot number of 1 or more'); continue; }
      if (x.slot > MAX_SLOTS) { warn('Slot above the limit', String(x.slot).slice(0, 20), 'runner-state lists slot ' + String(x.slot).slice(0, 20) + ', beyond the most the dashboard draws (' + MAX_SLOTS + '); it is not shown'); continue; }
      if (seenSlot.has(x.slot)) { warn('Slot listed twice', String(x.slot), 'runner-state slots has more than one entry for slot ' + x.slot); continue; }
      seenSlot.add(x.slot);
      if (x.role !== undefined && !ROLES.includes(x.role)) warn('Bad slot value', 'role ' + text(x.role), 'slot ' + x.slot + ': role must be one of ' + ROLES.join(', '));
      if (x.agent !== undefined && !AGENTS.includes(x.agent)) warn('Bad slot value', 'agent ' + text(x.agent), 'slot ' + x.slot + ': agent must be one of ' + AGENTS.join(', '));
      const slice = text(x.slice), doing = text(x.doing), since = text(x.since);
      const node = slice ? byId[slice] : undefined;
      // a slice with an open pull request (or a slice/<id> branch) is real work even when slices.yaml does not list it yet: a note, not an error
      const livePR = slice ? openPR.get(slice) : undefined, live = !!slice && (!!livePR || started.has(slice));
      if (slice && !node) {
        if (live) notice('Slot slice not in plan files yet', slice, 'slot ' + x.slot + ' names ' + slice + ', which runner/slices.yaml does not list, but ' + (livePR ? 'PR #' + livePR.number : 'branch slice/' + slice) + ' exists; add it to the plan files');
        else warn('Unknown slice', slice, 'slot ' + x.slot + ' in runner-state slots names a slice that is not in runner/slices.yaml, and has no open pull request or slice/ branch');
      }
      // a merged slice with a new open pull request (a follow-up on the same branch) is still being worked on
      const followUp = !!node && ['done', 'closed'].includes(node.state) && !!livePR;
      if (node && ['done', 'closed'].includes(node.state) && !followUp) warn('Slot lists finished work', slice, 'slot ' + x.slot + ' still shows a slice that has merged or closed; the runner should update slots');
      if (slice) { if (seenSlice.has(slice)) warn('Slice in two slots', slice, 'slots ' + seenSlice.get(slice) + ' and ' + x.slot + ' both list it'); else seenSlice.set(slice, x.slot); }
      rows[x.slot] = { slot: x.slot, status: x.status === 'idle' ? 'idle' : x.slot > cap ? 'finishing' : 'working', slice, title: node?.title || (livePR ? stripId(livePR.title) : ''), doing, agent: AGENTS.includes(x.agent) ? x.agent : node?.agent || (livePR ? agentOf(livePR) : ''), model: node?.model || '', role: ROLES.includes(x.role) ? x.role : 'build', since, stage: followUp ? 'active' : node?.state || (live ? 'active' : ''), pr: (followUp && livePR ? livePR.number : node?.pr) || livePR?.number || 0, asof: slotAsof };
    }
    for (let n = 1; n <= cap; n++) slots.push(rows[n] || { slot: n, status: 'idle', slice: '', title: '', doing: '', agent: '', model: '', role: '', since: '', stage: '', pr: 0, asof: slotAsof });
    // a session still running in a slot above the limit (the limit was lowered while it works) is shown as finishing, not hidden
    for (const n of Object.keys(rows).map(Number).filter((k) => k > cap).sort((a, b) => a - b)) slots.push(rows[n]);
    const t = slotAsof ? Date.parse(slotAsof) : NaN, age = (Date.parse(inp.now) - t) / 36e5;
    if (!slotAsof) warn('Slots date missing', 'none', 'runner-state slots.updated is missing, so the dashboard cannot tell how fresh the list is');
    else if (!Number.isFinite(t) || age < -0.1) warn('Slots date invalid', slotAsof.slice(0, 40), 'runner-state slots.updated is not a past UTC time; the runner should set it to the time of its last change');
    else if (age > 24) warn('Slots list is stale', slotAsof, 'runner-state slots.updated is ' + Math.round(age) + ' h old; the runner should refresh it on the wake where a session starts, finishes or changes task');
    const listed = new Set(items.map((x: any) => text(x?.slice)).filter(Boolean));
    // a pull request is in a slot when the slot names any slice it stands for: its title id or the slice its branch is named after
    const loose = inp.openPRs.filter((p) => openIdsOf(p).length && !openIdsOf(p).some((id) => listed.has(id))).map((p) => sliceIdsOf(p)[0] || '#' + p.number);
    if (loose.length) warn('Open work in no slot', String(loose.length), 'Open pull requests for ' + loose.slice(0, 8).join(', ') + (loose.length > 8 ? ', …' : '') + ' are not in any slot in runner-state slots');
    // free slots while work is ready: a slot is free when fewer than `limit` sessions run, and a slice is ready when it has no unmet
    // dependency and its `owns` do not overlap a running slice's. Reported as an `idle` row, apart from the plan-file warnings.
    const running = new Set<string>([...listed, ...Object.keys(openById)]);
    const stem = (g: string) => { const i = g.indexOf('*'); return (i < 0 ? g : g.slice(0, i)).replace(/\/+$/, ''); };
    const clash = (a: string[], b: string[]) => a.some((x) => b.some((y) => { const p = stem(x), q = stem(y); return p === q || p.startsWith(q + '/') || q.startsWith(p + '/') || !p || !q; }));
    const busyOwns = SL.filter((x) => running.has(x.id)).flatMap((x) => (Array.isArray(x.owns) ? x.owns.map(String) : []));
    const busyCount = slots.filter((r) => r.status !== 'idle').length;
    const free = cap ? Math.max(0, cap - busyCount) : 0;
    const fits = SL.filter((x) => state[x.id] === 'ready' && x.kind !== 'epic' && !running.has(x.id) && !clash(Array.isArray(x.owns) ? x.owns.map(String) : [], busyOwns))
      .map((x) => ({ id: x.id, unlocks: nodes.find((n) => n.id === x.id)?.unlocks || 0 })).sort((a, b) => b.unlocks - a.unlocks || (a.id < b.id ? -1 : 1));
    if (free > 0 && fits.length) idle.push({ label: 'Free slots, work ready', value: free + ' free, ' + fits.length + ' ready', note: 'Ready with no overlap with a running slice: ' + fits.slice(0, 6).map((f) => f.id + (f.unlocks ? ' (unlocks ' + f.unlocks + ')' : '')).join(', ') + (fits.length > 6 ? ', …' : '') });
  } else if (inp.runnerState) warn('Slots not recorded', 'none', 'runner-state state.json has no slots key, so the dashboard cannot show what each slot is doing');

  if (inp.historyIncomplete) warn('Merge history incomplete', 'yes', 'GitHub did not answer for some older pages of closed pull requests, so some finished slices may show as not yet merged until it does');
  if (unplaced.length) warn('Slices not placed', String(unplaced.length), 'No placement rule covers: ' + unplaced.slice(0, 12).join(', ') + (unplaced.length > 12 ? ', …' : '') + '. Add them to placement: in runner/dashboard.yaml.');
  const metaRows: any[] = [
    { key: 'main', label: 'Main branch', value: inp.mainSha.slice(0, 8), note: inp.mainDate },
    { key: 'state', label: 'Runner state last pushed', value: String(inp.runnerState?.updated_at || 'n/a'), note: 'runner-state/state.json (Claude runner only; optional)' },
    { key: 'graph', label: 'Plan files', value: 'main', note: 'runner/slices.yaml and runner/dashboard.yaml' },
    { key: 'built', label: 'Built at', value: inp.now.slice(0, 16).replace('T', ' ') + ' UTC', note: 'live, by the status connector' },
    ...warnings.map((w) => ({ key: 'warning', label: w.label, value: w.value, note: w.note })),
    ...notices.map((w) => ({ key: 'note', label: w.label, value: w.value, note: w.note })),
    ...idle.map((w) => ({ key: 'idle', label: w.label, value: w.value, note: w.note })),
  ];
  return { nodes, road, edges, activity, recent, meta: metaRows, gates, owner, ext, notes, crawler, soak, slots };
}
