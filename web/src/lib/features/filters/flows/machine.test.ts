import { describe, expect, it } from 'vitest';
import { ApiError } from '$lib/api/client';
import { coreSwitch, floorRemoval } from './changes';
import {
	DEFAULT_RETRY_S,
	FAST_POLL_MS,
	SLOW_POLL_MS,
	assessmentDiffers,
	forApply,
	initialCtx,
	transition,
	type Ctx,
	type Effect,
	type Event,
	type FlowState
} from './machine';
import { applied, finished, preview, running, strengthening, weakening } from './testing/fixtures';
import type { Change } from './types';

const T0 = 1_000_000;
const off = coreSwitch('CS-4a', false);

function run(ctx: Ctx, ...events: Event[]): { ctx: Ctx; effects: Effect[] } {
	let effects: Effect[] = [];
	for (const e of events) {
		const step = transition(ctx, e);
		ctx = step.ctx;
		effects = step.effects;
	}
	return { ctx, effects };
}

/** A flow that previewed `change` and got `assessment` back. */
function previewed(assessment = weakening(), change: Change = off): Ctx {
	const started = run(initialCtx, { type: 'start', change, now: T0 }).ctx;
	return run(
		started,
		{ type: 'job_started', seq: started.seq, job: 7 },
		{ type: 'job_status', seq: started.seq, status: finished(assessment), now: T0 + 300 }
	).ctx;
}

function applying(assessment = weakening(), password = 'hunter22'): Ctx {
	const ctx = previewed(assessment);
	return run(
		ctx,
		assessment.confirm ? { type: 'confirm', password, now: T0 + 400 } : { type: 'apply' }
	).ctx;
}

const err = (status: number, code: string, extra: ConstructorParameters<typeof ApiError>[3] = {}) =>
	new ApiError(status, code, `server says ${code}`, extra);

const kind = (s: FlowState) => s.kind;
const effectTypes = (fx: Effect[]) => fx.map((f) => f.type);
/** Every effect, serialized: what could ever reach the network or a log. */
const wire = (fx: Effect[]) => JSON.stringify(fx);

describe('start and the preview job', () => {
	it('starts a job with the change and no password, from idle or closed', () => {
		for (const from of [initialCtx, run(previewed(), { type: 'cancel' }).ctx]) {
			const { ctx, effects } = run(from, { type: 'start', change: off, now: T0 });
			expect(ctx.state).toEqual({ kind: 'previewing', progress: null });
			const job = effects.find((e) => e.type === 'start_job');
			expect(job).toEqual({ type: 'start_job', seq: ctx.seq, change: off });
			expect(wire(effects)).not.toContain('password');
		}
	});

	it('polls every 250 ms for the first 2 s, then every second, showing progress', () => {
		const started = run(initialCtx, { type: 'start', change: off, now: T0 }).ctx;
		const seq = started.seq;
		let step = run(started, { type: 'job_started', seq, job: 7 });
		expect(step.effects).toEqual([{ type: 'poll', seq, job: 7, delay: FAST_POLL_MS }]);
		step = run(step.ctx, { type: 'job_status', seq, status: running(), now: T0 + 1999 });
		expect(step.ctx.state).toEqual({ kind: 'previewing', progress: running().progress });
		expect(step.effects).toEqual([{ type: 'poll', seq, job: 7, delay: FAST_POLL_MS }]);
		step = run(step.ctx, { type: 'job_status', seq, status: running(), now: T0 + 2000 });
		expect(step.effects).toEqual([{ type: 'poll', seq, job: 7, delay: SLOW_POLL_MS }]);
	});

	it('goes to review when the server asks for no password, confirm when it does', () => {
		expect(previewed(strengthening()).state).toMatchObject({ kind: 'review', change: off });
		expect(previewed(weakening()).state).toMatchObject({ kind: 'confirm', change: off });
		// Only `confirm` decides: a weakening change the server doesn't gate goes to review.
		expect(kind(previewed(weakening({ confirm: false })).state)).toBe('review');
	});

	it('fails with the job error and status; a cancelled job closes', () => {
		const started = run(
			initialCtx,
			{ type: 'start', change: off, now: T0 },
			{ type: 'job_started', seq: 1, job: 7 }
		).ctx;
		const failed = run(started, {
			type: 'job_status',
			seq: started.seq,
			now: T0,
			status: {
				state: 'failed',
				status: 409,
				error: { code: 'floor_locked', message: 'CS-1a is part of the locked floor.' },
				progress: { phase: 'reading', done: 0, total: 0 }
			}
		}).ctx.state;
		expect(failed.kind).toBe('failed');
		if (failed.kind !== 'failed') return;
		expect([failed.error.status, failed.error.code, failed.error.message]).toEqual([
			409,
			'floor_locked',
			'CS-1a is part of the locked floor.'
		]);
		expect(failed.canRetry).toBe(false);
		const cancelled = run(started, {
			type: 'job_status',
			seq: started.seq,
			now: T0,
			status: { state: 'cancelled', progress: { phase: 'reading', done: 0, total: 0 } }
		});
		expect(kind(cancelled.ctx.state)).toBe('closed');
	});

	it('429 too_many_previews fails with Retry, and Retry previews again', () => {
		const started = run(initialCtx, { type: 'start', change: off, now: T0 }).ctx;
		const failed = run(started, {
			type: 'job_error',
			seq: started.seq,
			phase: 'start',
			error: err(429, 'too_many_previews'),
			now: T0
		}).ctx;
		expect(failed.state).toMatchObject({ kind: 'failed', canRetry: true });
		const again = run(failed, { type: 'retry', now: T0 + 5 });
		expect(kind(again.ctx.state)).toBe('previewing');
		expect(again.effects).toContainEqual({ type: 'start_job', seq: again.ctx.seq, change: off });
	});

	it('a job 404 restarts the preview once, then fails', () => {
		const started = run(
			initialCtx,
			{ type: 'start', change: off, now: T0 },
			{ type: 'job_started', seq: 1, job: 7 }
		).ctx;
		const gone = {
			type: 'job_error',
			phase: 'poll',
			error: err(404, 'preview_not_found'),
			now: T0
		} as const;
		const once = run(started, { ...gone, seq: started.seq });
		expect(kind(once.ctx.state)).toBe('previewing');
		expect(effectTypes(once.effects)).toContain('start_job');
		const twice = run(
			once.ctx,
			{ type: 'job_started', seq: once.ctx.seq, job: 8 },
			{ ...gone, seq: once.ctx.seq }
		);
		expect(twice.ctx.state).toMatchObject({ kind: 'failed' });
	});

	it('401 login_required closes (the client signs out)', () => {
		const started = run(initialCtx, { type: 'start', change: off, now: T0 }).ctx;
		const r = run(started, {
			type: 'job_error',
			seq: started.seq,
			phase: 'poll',
			error: err(401, 'login_required'),
			now: T0
		});
		expect(kind(r.ctx.state)).toBe('closed');
	});
});

describe('cancel and late answers', () => {
	it('cancel while previewing cancels the job and aborts the polls', () => {
		const started = run(
			initialCtx,
			{ type: 'start', change: off, now: T0 },
			{ type: 'job_started', seq: 1, job: 7 }
		).ctx;
		const r = run(started, { type: 'cancel' });
		expect(kind(r.ctx.state)).toBe('closed');
		expect(r.effects).toEqual([{ type: 'abort' }, { type: 'cancel_job', job: 7 }]);
	});

	it('a poll answer after cancel never changes state', () => {
		const started = run(
			initialCtx,
			{ type: 'start', change: off, now: T0 },
			{ type: 'job_started', seq: 1, job: 7 }
		).ctx;
		const closed = run(started, { type: 'cancel' }).ctx;
		const late = run(closed, {
			type: 'job_status',
			seq: started.seq,
			status: finished(weakening()),
			now: T0
		});
		expect(late.ctx).toBe(closed);
		expect(late.effects).toEqual([]);
	});

	it('a job named after its preview was abandoned is cancelled', () => {
		const started = run(initialCtx, { type: 'start', change: off, now: T0 }).ctx;
		const closed = run(started, { type: 'cancel' }).ctx;
		expect(run(closed, { type: 'job_started', seq: started.seq, job: 9 }).effects).toEqual([
			{ type: 'cancel_job', job: 9 }
		]);
	});

	it('an apply answer after cancel is ignored', () => {
		const ctx = applying();
		const closed = run(ctx, { type: 'cancel' }).ctx;
		expect(run(closed, { type: 'applied', seq: ctx.seq, response: applied(weakening()) }).ctx).toBe(
			closed
		);
	});

	it('cancel works from every state', () => {
		const states: Ctx[] = [
			run(initialCtx, { type: 'start', change: off, now: T0 }).ctx,
			previewed(strengthening()),
			previewed(weakening()),
			applying(),
			run(applying(), { type: 'applied', seq: applying().seq, response: applied(weakening()) }).ctx
		];
		for (const c of states) expect(kind(run(c, { type: 'cancel' }).ctx.state)).toBe('closed');
		expect(run(initialCtx, { type: 'cancel' }).ctx).toBe(initialCtx);
	});
});

describe('apply and confirm', () => {
	it('apply from review sends the change with no password key', () => {
		const r = run(previewed(strengthening()), { type: 'apply' });
		expect(kind(r.ctx.state)).toBe('applying');
		expect(r.effects).toEqual([{ type: 'apply', seq: r.ctx.seq, body: { change: off } }]);
		expect(Object.keys((r.effects[0] as Extract<Effect, { type: 'apply' }>).body)).toEqual([
			'change'
		]);
	});

	it('apply does nothing outside review; confirm does nothing outside confirm', () => {
		for (const c of [initialCtx, previewed(weakening()), applying()]) {
			expect(run(c, { type: 'apply' }).effects).toEqual([]);
		}
		for (const c of [initialCtx, previewed(strengthening()), applying()]) {
			expect(run(c, { type: 'confirm', password: 'x', now: T0 }).effects).toEqual([]);
		}
	});

	it('confirm sends the password only in the apply body', () => {
		const r = run(previewed(weakening()), { type: 'confirm', password: 'hunter22', now: T0 });
		expect(r.effects).toEqual([
			{ type: 'apply', seq: r.ctx.seq, body: { change: off, password: 'hunter22' } }
		]);
		expect(JSON.stringify(r.ctx)).not.toContain('hunter22');
	});

	it('an empty password never sends', () => {
		const r = run(previewed(weakening()), { type: 'confirm', password: '', now: T0 });
		expect(r.effects).toEqual([]);
		expect(r.ctx.state).toMatchObject({ kind: 'confirm', error: 'empty' });
	});

	it('a second confirm while applying sends nothing more', () => {
		const ctx = applying();
		expect(run(ctx, { type: 'confirm', password: 'again', now: T0 }).effects).toEqual([]);
		expect(run(ctx, { type: 'apply' }).effects).toEqual([]);
	});

	it('200 → done with what was previewed', () => {
		const ctx = applying();
		const res = applied(weakening({ preview: preview({ quarantine: 4 }) }), { released: 4 });
		const r = run(ctx, { type: 'applied', seq: ctx.seq, response: res });
		expect(r.ctx.state).toEqual({ kind: 'done', response: res, previewed: weakening() });
	});
});

describe('apply outcomes', () => {
	const outcome = (e: ApiError, from = applying()) =>
		run(from, { type: 'apply_error', seq: from.seq, error: e, now: T0 + 1000 });

	it('428 with a different assessment → confirm, changed, nothing re-sent', () => {
		const next = weakening({ warning: 'Now it releases 12 torrents.' });
		const r = outcome(
			err(428, 'confirm_required', { assessment: next }),
			applying(strengthening())
		);
		expect(r.ctx.state).toEqual({ kind: 'confirm', change: off, assessment: next, changed: true });
		expect(r.effects).toEqual([]);
	});

	it('428 with the same assessment still needs a new confirm, unchanged', () => {
		const r = outcome(err(428, 'confirm_required', { assessment: weakening() }));
		expect(r.ctx.state).toEqual({ kind: 'confirm', change: off, assessment: weakening() });
		expect(r.effects).toEqual([]);
	});

	it('401 bad_password → confirm with the error, no sign-out', () => {
		const r = outcome(err(401, 'bad_password'));
		expect(r.ctx.state).toMatchObject({ kind: 'confirm', error: 'bad_password' });
		expect(r.effects).toEqual([]);
	});

	it('429 rate_limited → confirm with retryAt; no request until it passes', () => {
		const withHeader = outcome(err(429, 'rate_limited', { retryAfter: 12 })).ctx;
		expect(withHeader.state).toMatchObject({ error: 'rate_limited', retryAt: T0 + 1000 + 12_000 });
		const locked = outcome(err(429, 'rate_limited')).ctx;
		const retryAt = T0 + 1000 + DEFAULT_RETRY_S * 1000;
		expect(locked.state).toMatchObject({ retryAt });
		expect(run(locked, { type: 'confirm', password: 'pw', now: retryAt - 1 }).effects).toEqual([]);
		const sent = run(locked, { type: 'confirm', password: 'pw', now: retryAt });
		expect(effectTypes(sent.effects)).toEqual(['apply']);
	});

	it('409 changed_since_preview → a new preview job', () => {
		const r = outcome(err(409, 'changed_since_preview'));
		expect(kind(r.ctx.state)).toBe('previewing');
		expect(r.effects).toContainEqual({ type: 'start_job', seq: r.ctx.seq, change: off });
		expect(wire(r.effects)).not.toContain('password');
	});

	it('401 login_required → closed', () => {
		expect(kind(outcome(err(401, 'login_required')).ctx.state)).toBe('closed');
	});

	it.each([
		[409, 'floor_locked', false],
		[400, 'invalid_override', false],
		[422, 'whatever', false],
		[500, 'internal', true],
		[0, 'network', true]
	])('%i %s → failed (retry: %s)', (status, code, retry) => {
		const r = outcome(err(status, code));
		expect(r.ctx.state).toMatchObject({ kind: 'failed', canRetry: retry });
		if (r.ctx.state.kind === 'failed')
			expect(r.ctx.state.error.message).toBe(`server says ${code}`);
	});

	it('Retry after a failed apply starts from the preview', () => {
		const failed = outcome(err(500, 'internal')).ctx;
		const r = run(failed, { type: 'retry', now: T0 });
		expect(kind(r.ctx.state)).toBe('previewing');
		expect(effectTypes(r.effects)).toContain('start_job');
	});

	it('floor_removal_needs_hashes is a client bug: failed plus a dev error', () => {
		const r = outcome(err(400, 'floor_removal_needs_hashes'));
		expect(kind(r.ctx.state)).toBe('failed');
		expect(effectTypes(r.effects)).toEqual(['dev_error']);
	});
});

describe('floor removals', () => {
	const remove = floorRemoval('file:/config/blocklist.txt', true);
	const shown = weakening({ floor_entries: ['a'.repeat(40), 'b'.repeat(40)] });

	it('send exactly the floor_entries of the assessment on screen', () => {
		const ctx = previewed(shown, remove);
		const r = run(ctx, { type: 'confirm', password: 'pw', now: T0 });
		expect(r.effects).toEqual([
			{
				type: 'apply',
				seq: ctx.seq,
				body: { change: { ...remove, hashes: shown.floor_entries }, password: 'pw' }
			}
		]);
	});

	it('after a 428 with more entries, confirm sends the new ones the user now sees', () => {
		const ctx = run(previewed(shown, remove), { type: 'confirm', password: 'pw', now: T0 }).ctx;
		const wider = weakening({
			floor_entries: [...shown.floor_entries!, 'c'.repeat(40)],
			warning: 'More.'
		});
		const back = run(ctx, {
			type: 'apply_error',
			seq: ctx.seq,
			error: err(428, 'confirm_required', { assessment: wider }),
			now: T0
		}).ctx;
		const r = run(back, { type: 'confirm', password: 'pw', now: T0 });
		const body = (r.effects[0] as Extract<Effect, { type: 'apply' }>).body;
		expect(body.change).toEqual({ ...remove, hashes: wider.floor_entries });
	});

	it('keeping, or a removal the server listed nothing for, sends no hashes', () => {
		const keep = floorRemoval('list:3', false);
		expect(forApply(keep, shown)).toEqual(keep);
		expect(forApply(remove, weakening())).toEqual(remove);
	});
});

describe('variants', () => {
	it('re-preview from review or confirm with the new change', () => {
		const shadowed = { op: 'add_rule', query: 'x', action: 'hide', shadow_days: 7 } as const;
		for (const c of [previewed(strengthening()), previewed(weakening())]) {
			const r = run(c, { type: 'variant', change: shadowed, now: T0 });
			expect(kind(r.ctx.state)).toBe('previewing');
			expect(r.effects).toContainEqual({ type: 'start_job', seq: r.ctx.seq, change: shadowed });
		}
		expect(run(applying(), { type: 'variant', change: off, now: T0 }).effects).toEqual([]);
	});

	it('start while applying is ignored', () => {
		const ctx = applying();
		expect(run(ctx, { type: 'start', change: off, now: T0 }).ctx).toBe(ctx);
	});

	it('start while previewing cancels the running job first', () => {
		const started = run(
			initialCtx,
			{ type: 'start', change: off, now: T0 },
			{ type: 'job_started', seq: 1, job: 7 }
		).ctx;
		const r = run(started, { type: 'start', change: coreSwitch('CS-2a', false), now: T0 });
		expect(r.effects.slice(0, 2)).toEqual([{ type: 'abort' }, { type: 'cancel_job', job: 7 }]);
	});
});

describe('assessmentDiffers', () => {
	it('compares the warning and every preview number', () => {
		expect(assessmentDiffers(weakening(), weakening())).toBe(false);
		expect(assessmentDiffers(weakening(), weakening({ warning: 'x' }))).toBe(true);
		expect(assessmentDiffers(weakening(), weakening({ preview: preview({ per_day: 8 }) }))).toBe(
			true
		);
	});
});

describe('the full state × event table', () => {
	const doneCtx = () => {
		const c = applying();
		return run(c, { type: 'applied', seq: c.seq, response: applied(weakening()) }).ctx;
	};
	const failedCtx = () => {
		const c = applying();
		return run(c, { type: 'apply_error', seq: c.seq, error: err(500, 'internal'), now: T0 }).ctx;
	};
	const previewingCtx = () =>
		run(
			initialCtx,
			{ type: 'start', change: off, now: T0 },
			{ type: 'job_started', seq: 1, job: 7 }
		).ctx;
	const states: Record<FlowState['kind'], () => Ctx> = {
		idle: () => initialCtx,
		closed: () => run(previewed(), { type: 'cancel' }).ctx,
		previewing: previewingCtx,
		review: () => previewed(strengthening()),
		confirm: () => previewed(weakening()),
		applying: () => applying(),
		done: doneCtx,
		failed: failedCtx
	};
	// Events as they'd arrive for the context's own (current) generation.
	const events = (c: Ctx): Record<string, Event> => ({
		start: { type: 'start', change: off, now: T0 },
		variant: { type: 'variant', change: off, now: T0 },
		retry: { type: 'retry', now: T0 },
		cancel: { type: 'cancel' },
		job_status: { type: 'job_status', seq: c.seq, status: running(), now: T0 },
		job_error: { type: 'job_error', seq: c.seq, error: err(500, 'x'), phase: 'poll', now: T0 },
		apply: { type: 'apply' },
		confirm: { type: 'confirm', password: 'pw', now: T0 },
		applied: { type: 'applied', seq: c.seq, response: applied(weakening()) },
		apply_error: { type: 'apply_error', seq: c.seq, error: err(500, 'x'), now: T0 }
	});
	// The state each event leads to; absent = ignored (the same context, no effects).
	const table: Record<FlowState['kind'], Partial<Record<string, FlowState['kind']>>> = {
		idle: { start: 'previewing' },
		closed: { start: 'previewing' },
		previewing: {
			start: 'previewing',
			cancel: 'closed',
			job_status: 'previewing',
			job_error: 'failed'
		},
		review: { start: 'previewing', variant: 'previewing', cancel: 'closed', apply: 'applying' },
		confirm: { start: 'previewing', variant: 'previewing', cancel: 'closed', confirm: 'applying' },
		applying: { cancel: 'closed', applied: 'done', apply_error: 'failed' },
		done: { start: 'previewing', cancel: 'closed' },
		failed: { start: 'previewing', retry: 'previewing', cancel: 'closed' }
	};

	for (const [kind, make] of Object.entries(states) as [FlowState['kind'], () => Ctx][]) {
		for (const [name, event] of Object.entries(events(make()))) {
			const target = table[kind][name];
			it(`${kind} × ${name} → ${target ?? 'ignored'}`, () => {
				const c = make();
				const r = transition(c, event);
				if (target === undefined) {
					expect(r.ctx).toBe(c);
					expect(r.effects).toEqual([]);
				} else {
					expect(r.ctx.state.kind).toBe(target);
				}
				// Only a confirm, from `confirm`, may carry the password.
				if (!(kind === 'confirm' && name === 'confirm'))
					expect(wire(r.effects)).not.toContain('"password"');
			});
		}
	}

	it('a late answer for an older generation is ignored in every state', () => {
		for (const make of Object.values(states)) {
			const c = make();
			for (const e of Object.values(events({ ...c, seq: c.seq - 1 }))) {
				if (!('seq' in e)) continue;
				expect(transition(c, e).ctx).toBe(c);
			}
		}
	});
});

describe('the rate-limit lock', () => {
	it('holds across a re-preview: a variant does not reopen the confirm early', () => {
		const ctx = applying();
		const locked = run(ctx, {
			type: 'apply_error',
			seq: ctx.seq,
			error: err(429, 'rate_limited', { retryAfter: 30 }),
			now: T0
		}).ctx;
		const again = run(
			locked,
			{ type: 'variant', change: off, now: T0 + 1 },
			{ type: 'job_started', seq: locked.seq + 1, job: 8 }
		).ctx;
		const shown = run(again, {
			type: 'job_status',
			seq: again.seq,
			status: finished(weakening()),
			now: T0 + 2
		}).ctx;
		expect(shown.state).toMatchObject({
			kind: 'confirm',
			error: 'rate_limited',
			retryAt: T0 + 30_000
		});
		expect(run(shown, { type: 'confirm', password: 'pw', now: T0 + 29_999 }).effects).toEqual([]);
		expect(
			effectTypes(run(shown, { type: 'confirm', password: 'pw', now: T0 + 30_000 }).effects)
		).toEqual(['apply']);
	});
});

describe('the "effect changed" notice', () => {
	it('stays through a wrong password and an empty one', () => {
		const from = applying(strengthening());
		const back = run(from, {
			type: 'apply_error',
			seq: from.seq,
			error: err(428, 'confirm_required', { assessment: weakening() }),
			now: T0
		}).ctx;
		expect(back.state).toMatchObject({ changed: true });
		const empty = run(back, { type: 'confirm', password: '', now: T0 }).ctx;
		expect(empty.state).toMatchObject({ error: 'empty', changed: true });
		const sent = run(back, { type: 'confirm', password: 'wrong', now: T0 }).ctx;
		const refused = run(sent, {
			type: 'apply_error',
			seq: sent.seq,
			error: err(401, 'bad_password'),
			now: T0
		}).ctx;
		expect(refused.state).toMatchObject({ error: 'bad_password', changed: true });
	});
});
