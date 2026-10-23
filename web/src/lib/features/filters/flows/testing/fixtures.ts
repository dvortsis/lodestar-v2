// Fixtures for the flows' tests. Preview values are chosen so a sum of any two
// rows is never another row's value (a summed row would show).
import type { ApplyResponse, Assessment, JobStatus, Preview } from '../types';

export const preview = (over: Partial<Preview> = {}): Preview => ({
	quarantine: 3,
	tombstones: 17,
	stored: 120,
	removed_from_floor: 5,
	caught_30d: 214,
	core_caught_30d: 9,
	per_day: 7,
	estimate: false,
	...over
});

export const WARNING =
	'Turning off CS-4a stops that child-safety check for every torrent. Other core rules keep checking.';

export const weakening = (over: Partial<Assessment> = {}): Assessment => ({
	weakening: true,
	confirm: true,
	warning: WARNING,
	preview: preview(),
	neutralize: null,
	...over
});

export const strengthening = (over: Partial<Assessment> = {}): Assessment => ({
	weakening: false,
	confirm: false,
	warning: null,
	preview: preview({
		quarantine: 0,
		tombstones: 0,
		stored: 0,
		caught_30d: 0,
		core_caught_30d: 0,
		per_day: 0
	}),
	neutralize: null,
	...over
});

export const running = (done = 40, total = 100): JobStatus => ({
	state: 'running',
	progress: { phase: 'quarantine', done, total }
});

export const finished = (assessment: Assessment): JobStatus => ({
	state: 'done',
	progress: { phase: 'done', done: 1, total: 1 },
	assessment
});

export const applied = (
	assessment: Assessment,
	over: Partial<ApplyResponse> = {}
): ApplyResponse => ({
	applied: true,
	assessment,
	version: 12,
	...over
});

/** Strings no output may render as markup: tags, an event handler, a bidi override. */
export const HOSTILE = '<img src=x onerror="alert(1)"><script>alert(2)</script>‮evil';
