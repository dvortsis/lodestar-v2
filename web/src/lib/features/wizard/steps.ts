// The wizard's steps and its progress (§10.10.10). Steps 1 and 2 happen in /setup; this list holds
// the rest. Steps 6 (import from bitmagnet), 7 (download client) and the rule-pack step (§20.6) are
// part 2 (docs/decisions/2026-10-31-S-wizard.md): `SLOTS` names where they go, so adding one is a
// new entry here and a new loader in the shell, nothing else.

export type StepId = 'network' | 'how' | 'intensity' | 'storage' | 'updates' | 'done';

/** Where part 2's steps slot in, by the step they follow. Nothing renders from this yet. */
export const SLOTS = {
	import: 'storage',
	'download-client': 'import',
	'rule-packs': 'download-client'
} as const;

export interface StepInfo {
	id: StepId;
	/** An optional step has Skip, which sends nothing. */
	optional: boolean;
	/** The Settings screen that holds what a skipped step would have set. */
	settings: string;
}

export const STEPS: readonly StepInfo[] = [
	{ id: 'network', optional: true, settings: 'network' },
	{ id: 'how', optional: false, settings: 'general' },
	{ id: 'intensity', optional: true, settings: 'crawler' },
	{ id: 'storage', optional: true, settings: 'storage' },
	{ id: 'updates', optional: true, settings: 'updates' },
	{ id: 'done', optional: false, settings: 'general' }
];

export const stepIndex = (id: StepId) => STEPS.findIndex((s) => s.id === id);

export interface Progress {
	step: StepId;
	skipped: StepId[];
	done: StepId[];
}

const KEY = 'lodestar.wizard';
const fresh = (): Progress => ({ step: STEPS[0]!.id, skipped: [], done: [] });

const isId = (v: unknown): v is StepId => STEPS.some((s) => s.id === v);
const ids = (v: unknown): StepId[] => (Array.isArray(v) ? v.filter(isId) : []);

/** Progress from `sessionStorage`, so a reload resumes. Anything unreadable starts over. */
export function readProgress(): Progress {
	try {
		const raw: unknown = JSON.parse(sessionStorage.getItem(KEY) ?? 'null');
		if (!raw || typeof raw !== 'object') return fresh();
		const r = raw as Record<string, unknown>;
		return isId(r.step) ? { step: r.step, skipped: ids(r.skipped), done: ids(r.done) } : fresh();
	} catch {
		return fresh();
	}
}

export function writeProgress(p: Progress): void {
	try {
		sessionStorage.setItem(KEY, JSON.stringify(p));
	} catch {
		/* Blocked storage only loses the resume. */
	}
}

const without = (list: StepId[], id: StepId) => list.filter((x) => x !== id);
const with_ = (list: StepId[], id: StepId) => (list.includes(id) ? list : [...list, id]);

/** Moves to the step after `p.step`, marking this one done (saved or just read) or skipped. */
export function advance(p: Progress, how: 'done' | 'skipped'): Progress {
	const next = STEPS[Math.min(stepIndex(p.step) + 1, STEPS.length - 1)]!.id;
	return {
		step: next,
		done: how === 'done' ? with_(p.done, p.step) : without(p.done, p.step),
		skipped: how === 'skipped' ? with_(p.skipped, p.step) : without(p.skipped, p.step)
	};
}

export function back(p: Progress): Progress {
	return { ...p, step: STEPS[Math.max(stepIndex(p.step) - 1, 0)]!.id };
}
