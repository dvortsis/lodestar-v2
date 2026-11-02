// The wizard's steps and its progress (§10.10.10). Steps 1 and 2 happen in /setup; this list holds
// the rest. Step 6 (import from bitmagnet) is still a hidden slot (online import is deferred,
// B-import-server-2): `SLOTS` names where it goes, so adding it is a new entry here and a new
// loader in the shell, nothing else (docs/decisions/2026-10-31-S-wizard.md).

export type StepId =
	| 'network'
	| 'how'
	| 'intensity'
	| 'storage'
	| 'download'
	| 'packs'
	| 'blocklists'
	| 'updates'
	| 'done';

/** Where the hidden import step slots in, by the step it follows. Nothing renders from this. */
export const SLOTS = { import: 'storage' } as const;

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
	{ id: 'download', optional: true, settings: 'integrations' },
	{ id: 'packs', optional: true, settings: 'filters' },
	{ id: 'blocklists', optional: true, settings: 'blocklists' },
	{ id: 'updates', optional: true, settings: 'updates' },
	{ id: 'done', optional: false, settings: 'general' }
];

export const stepIndex = (id: StepId) => STEPS.findIndex((s) => s.id === id);

export interface Progress {
	step: StepId;
	skipped: StepId[];
	done: StepId[];
	/** A saved field applies only after a restart; Done says so. */
	restart?: boolean;
}

const KEY = 'lodestar.wizard';
const fresh = (): Progress => ({ step: STEPS[0]!.id, skipped: [], done: [], restart: false });

const isId = (v: unknown): v is StepId => STEPS.some((s) => s.id === v);
const ids = (v: unknown): StepId[] => (Array.isArray(v) ? v.filter(isId) : []);

/** Progress from `sessionStorage`, so a reload resumes. Anything unreadable starts over. */
export function readProgress(): Progress {
	try {
		const raw: unknown = JSON.parse(sessionStorage.getItem(KEY) ?? 'null');
		if (!raw || typeof raw !== 'object') return fresh();
		const r = raw as Record<string, unknown>;
		return isId(r.step)
			? { step: r.step, skipped: ids(r.skipped), done: ids(r.done), restart: r.restart === true }
			: fresh();
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
export function advance(p: Progress, how: 'done' | 'skipped', restart = false): Progress {
	const next = STEPS[Math.min(stepIndex(p.step) + 1, STEPS.length - 1)]!.id;
	return {
		step: next,
		restart: p.restart === true || restart,
		done: how === 'done' ? with_(p.done, p.step) : without(p.done, p.step),
		skipped: how === 'skipped' ? with_(p.skipped, p.step) : without(p.skipped, p.step)
	};
}

export function back(p: Progress): Progress {
	return { ...p, step: STEPS[Math.max(stepIndex(p.step) - 1, 0)]!.id };
}
