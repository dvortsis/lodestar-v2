// The change flow's state machine (architecture §20.10, §20.11, §20.7 "As built"), pure: every
// transition is `transition(ctx, event) → { ctx, effects }`, so the whole table is testable without
// a DOM or a network. `flow.svelte.ts` runs the effects.
//
// The server decides. Whether a change weakens filtering, needs the password, is allowed at all,
// and what it does, all come from the preview job and `POST /filters/apply`. Nothing here judges a
// change: `confirm` is the assessment's own flag, and the warning is the server's text.
//
//   idle | closed ──start──▶ previewing ──job done, confirm: false──▶ review ──apply──▶ applying
//                               │        ──job done, confirm: true───▶ confirm ──confirm(pw)──▶ applying
//                               │        ──job failed / 429─────────▶ failed ──retry──▶ previewing
//                               │        ──job 404 (once)──▶ previewing (a new job)
//                               └──cancel / job cancelled──▶ closed
//   applying ──200──▶ done
//            ──428──▶ confirm (the server's new assessment; `changed` when it differs)
//            ──401 bad_password / 429 rate_limited──▶ confirm (error; retryAt)
//            ──409 changed_since_preview──▶ previewing (a new job)
//            ──401 login_required──▶ closed (the client's login handler signs out)
//            ──anything else──▶ failed
//   review | confirm ──variant──▶ previewing          any state ──cancel──▶ closed
//
// The password exists only in a `confirm` event and the `apply` effect it produces: it is never
// part of `Ctx`, a state or any other effect.
import { ApiError } from '$lib/api/client';
import type {
	Assessment,
	ApplyResponse,
	Change,
	ChangeBody,
	JobProgress,
	JobStatus
} from './types';

export type PasswordError = 'empty' | 'bad_password' | 'rate_limited';

export type FlowState =
	| { kind: 'idle' }
	| { kind: 'closed' }
	| { kind: 'previewing'; progress: JobProgress | null }
	| { kind: 'review'; change: Change; assessment: Assessment }
	| {
			kind: 'confirm';
			change: Change;
			assessment: Assessment;
			error?: PasswordError;
			/** Epoch ms before which no confirm is sent (429 `rate_limited`). */
			retryAt?: number;
			/** A 428 came back with a different warning or preview than the one shown. */
			changed?: boolean;
	  }
	| {
			kind: 'applying';
			change: Change;
			assessment: Assessment;
			/** Carried from `confirm`, so a refused password keeps the "effect changed" notice. */
			changed?: boolean;
	  }
	| { kind: 'done'; response: ApplyResponse; previewed: Assessment }
	| { kind: 'failed'; error: ApiError; canRetry: boolean };

export interface Ctx {
	readonly state: FlowState;
	/** The change being previewed or applied, as the caller gave it (no floor hashes added). */
	readonly change: Change | null;
	/** The running preview job, once the server named it. */
	readonly job: number | null;
	/** Bumped whenever a new request chain starts or the flow closes; late answers carry an old one. */
	readonly seq: number;
	/** The one restart after a job 404 was used. */
	readonly restarted: boolean;
	/** When the current preview started (epoch ms), for the poll cadence. */
	readonly startedAt: number;
	/** A 429 `rate_limited` holds every confirm until then (epoch ms), across re-previews. */
	readonly lockedUntil: number;
}

export type Effect =
	| { type: 'start_job'; seq: number; change: Change }
	| { type: 'poll'; seq: number; job: number; delay: number }
	| { type: 'cancel_job'; job: number }
	/** Abort in-flight polls and timers of earlier chains. */
	| { type: 'abort' }
	| { type: 'apply'; seq: number; body: ChangeBody }
	/** A client bug the server caught (development builds log it). */
	| { type: 'dev_error'; message: string };

export type Event =
	| { type: 'start'; change: Change; now: number }
	| { type: 'variant'; change: Change; now: number }
	| { type: 'retry'; now: number }
	| { type: 'cancel' }
	| { type: 'job_started'; seq: number; job: number }
	| { type: 'job_status'; seq: number; status: JobStatus; now: number }
	| { type: 'job_error'; seq: number; error: unknown; phase: 'start' | 'poll'; now: number }
	| { type: 'apply' }
	| { type: 'confirm'; password: string; now: number }
	| { type: 'applied'; seq: number; response: ApplyResponse }
	| { type: 'apply_error'; seq: number; error: unknown; now: number };

export interface Step {
	ctx: Ctx;
	effects: Effect[];
}

/** Poll every 250 ms for the first 2 s of a preview, then every second. */
export const FAST_POLL_MS = 250;
export const FAST_POLL_FOR_MS = 2000;
export const SLOW_POLL_MS = 1000;
/** A 429 `rate_limited` without `Retry-After` waits this long (the login bucket refills one per 12 s). */
export const DEFAULT_RETRY_S = 60;

export const initialCtx: Ctx = {
	state: { kind: 'idle' },
	change: null,
	job: null,
	seq: 0,
	restarted: false,
	startedAt: 0,
	lockedUntil: 0
};

export const pollDelay = (startedAt: number, now: number) =>
	now - startedAt < FAST_POLL_FOR_MS ? FAST_POLL_MS : SLOW_POLL_MS;

/** Any thrown value as an `ApiError` (a non-API throw reads as a network failure). */
export function asApiError(e: unknown): ApiError {
	if (e instanceof ApiError) return e;
	return new ApiError(0, 'network', e instanceof Error ? e.message : String(e));
}

/** Errors worth retrying unchanged: the network, the server, or a busy queue. */
export const canRetry = (e: ApiError) =>
	e.status === 0 || e.status === 408 || e.status === 429 || e.status >= 500;

/**
 * The change exactly as apply sends it. A floor removal names the entries the shown assessment
 * counted (`floor_entries`), so a refresh in between can't widen what the password confirms.
 */
export function forApply(change: Change, assessment: Assessment): Change {
	if (change.op !== 'resolve_floor_removal' || change.remove !== true) return change;
	const rest = { ...change };
	delete rest.hashes;
	return assessment.floor_entries ? { ...rest, hashes: [...assessment.floor_entries] } : rest;
}

const PREVIEW_KEYS = [
	'quarantine',
	'tombstones',
	'stored',
	'removed_from_floor',
	'caught_30d',
	'core_caught_30d',
	'per_day',
	'estimate'
] as const;

/** Whether what the user would confirm changed: the warning, any number, or the floor entries. */
export function assessmentDiffers(a: Assessment, b: Assessment): boolean {
	if (a.warning !== b.warning || a.confirm !== b.confirm) return true;
	if (PREVIEW_KEYS.some((k) => a.preview[k] !== b.preview[k])) return true;
	if (!!a.neutralize !== !!b.neutralize) return true;
	if (
		a.neutralize &&
		b.neutralize &&
		PREVIEW_KEYS.some((k) => a.neutralize![k] !== b.neutralize![k])
	)
		return true;
	return (a.floor_entries ?? []).join() !== (b.floor_entries ?? []).join();
}

const same = (ctx: Ctx): Step => ({ ctx, effects: [] });

function closed(ctx: Ctx, effects: Effect[] = []): Step {
	return {
		ctx: { ...ctx, state: { kind: 'closed' }, change: null, job: null, seq: ctx.seq + 1 },
		effects
	};
}

/** Stop whatever preview is running: abort the polls, and cancel the job if it has an id. */
function stopPreview(ctx: Ctx): Effect[] {
	if (ctx.state.kind !== 'previewing') return [];
	return ctx.job === null
		? [{ type: 'abort' }]
		: [{ type: 'abort' }, { type: 'cancel_job', job: ctx.job }];
}

function preview(ctx: Ctx, change: Change, now: number, restarted = false): Step {
	const seq = ctx.seq + 1;
	const stop = stopPreview(ctx);
	return {
		ctx: {
			state: { kind: 'previewing', progress: null },
			change,
			job: null,
			seq,
			restarted,
			startedAt: now,
			lockedUntil: ctx.lockedUntil
		},
		effects: [
			...(stop.length ? stop : [{ type: 'abort' } as const]),
			{ type: 'start_job', seq, change }
		]
	};
}

function failed(ctx: Ctx, error: ApiError, effects: Effect[] = []): Step {
	return {
		ctx: { ...ctx, state: { kind: 'failed', error, canRetry: canRetry(error) }, job: null },
		effects
	};
}

function fromJob(status: JobStatus): ApiError {
	const e = status.error;
	return new ApiError(status.status ?? 500, e?.code ?? 'preview_failed', e?.message ?? '', {
		field: e?.field
	});
}

export function transition(ctx: Ctx, event: Event): Step {
	const s = ctx.state;
	switch (event.type) {
		case 'start':
			// A change being applied is never abandoned for another one.
			if (s.kind === 'applying') return same(ctx);
			return preview(ctx, event.change, event.now);

		case 'variant':
			if (s.kind !== 'review' && s.kind !== 'confirm') return same(ctx);
			return preview(ctx, event.change, event.now);

		case 'retry':
			if (s.kind !== 'failed' || !ctx.change) return same(ctx);
			return preview(ctx, ctx.change, event.now);

		case 'cancel':
			if (s.kind === 'idle' || s.kind === 'closed') return same(ctx);
			return closed(ctx, stopPreview(ctx));

		case 'job_started':
			// A job for an abandoned preview still holds one of the server's four slots: cancel it.
			if (event.seq !== ctx.seq || s.kind !== 'previewing') {
				return { ctx, effects: [{ type: 'cancel_job', job: event.job }] };
			}
			return {
				ctx: { ...ctx, job: event.job },
				effects: [{ type: 'poll', seq: ctx.seq, job: event.job, delay: FAST_POLL_MS }]
			};

		case 'job_status': {
			if (event.seq !== ctx.seq || s.kind !== 'previewing' || ctx.job === null) return same(ctx);
			const st = event.status;
			switch (st.state) {
				case 'running':
					return {
						ctx: { ...ctx, state: { kind: 'previewing', progress: st.progress } },
						effects: [
							{
								type: 'poll',
								seq: ctx.seq,
								job: ctx.job,
								delay: pollDelay(ctx.startedAt, event.now)
							}
						]
					};
				case 'done': {
					const change = ctx.change!;
					const assessment = st.assessment;
					if (!assessment) {
						return failed(
							ctx,
							new ApiError(500, 'bad_response', 'The preview returned no result.')
						);
					}
					const locked = ctx.lockedUntil > event.now;
					const state: FlowState = !assessment.confirm
						? { kind: 'review', change, assessment }
						: locked
							? {
									kind: 'confirm',
									change,
									assessment,
									error: 'rate_limited',
									retryAt: ctx.lockedUntil
								}
							: { kind: 'confirm', change, assessment };
					return { ctx: { ...ctx, state, job: null }, effects: [] };
				}
				case 'failed':
					return failed(ctx, fromJob(st));
				case 'cancelled':
					return closed(ctx);
			}
			return same(ctx);
		}

		case 'job_error': {
			if (event.seq !== ctx.seq || s.kind !== 'previewing') return same(ctx);
			const e = asApiError(event.error);
			if (event.phase === 'poll' && e.status === 404 && !ctx.restarted && ctx.change) {
				// The job's result expired (kept 10 minutes) or was evicted: preview again, once.
				return preview(ctx, ctx.change, event.now, true);
			}
			if (e.status === 401 && e.code === 'login_required') return closed(ctx, [{ type: 'abort' }]);
			return failed(ctx, e, [{ type: 'abort' }]);
		}

		case 'apply':
			if (s.kind !== 'review') return same(ctx);
			return {
				ctx: { ...ctx, state: { kind: 'applying', change: s.change, assessment: s.assessment } },
				// No `password` key at all: the server said this change needs none.
				effects: [
					{ type: 'apply', seq: ctx.seq, body: { change: forApply(s.change, s.assessment) } }
				]
			};

		case 'confirm': {
			if (s.kind !== 'confirm') return same(ctx);
			if (event.now < Math.max(s.retryAt ?? 0, ctx.lockedUntil)) return same(ctx);
			if (event.password === '') {
				const state: FlowState = {
					kind: 'confirm',
					change: s.change,
					assessment: s.assessment,
					error: 'empty',
					...(s.changed ? { changed: true } : {})
				};
				return { ctx: { ...ctx, state }, effects: [] };
			}
			return {
				ctx: {
					...ctx,
					state: {
						kind: 'applying',
						change: s.change,
						assessment: s.assessment,
						...(s.changed ? { changed: true } : {})
					}
				},
				effects: [
					{
						type: 'apply',
						seq: ctx.seq,
						body: { change: forApply(s.change, s.assessment), password: event.password }
					}
				]
			};
		}

		case 'applied':
			if (event.seq !== ctx.seq || s.kind !== 'applying') return same(ctx);
			return {
				ctx: {
					...ctx,
					state: { kind: 'done', response: event.response, previewed: s.assessment }
				},
				effects: []
			};

		case 'apply_error': {
			if (event.seq !== ctx.seq || s.kind !== 'applying') return same(ctx);
			const e = asApiError(event.error);
			const { change, assessment } = s;
			if (e.status === 428 && e.code === 'confirm_required') {
				// The server re-assessed and wants the password. Never re-sent automatically.
				const next = e.assessment ?? assessment;
				const changed = e.assessment ? assessmentDiffers(assessment, e.assessment) : false;
				return {
					ctx: {
						...ctx,
						state: { kind: 'confirm', change, assessment: next, ...(changed ? { changed } : {}) }
					},
					effects: []
				};
			}
			if (e.status === 401 && e.code === 'bad_password') {
				return {
					ctx: {
						...ctx,
						state: {
							kind: 'confirm',
							change,
							assessment,
							error: 'bad_password',
							...(s.changed ? { changed: true } : {})
						}
					},
					effects: []
				};
			}
			if (e.status === 429 && e.code === 'rate_limited') {
				const retryAt = event.now + (e.retryAfter ?? DEFAULT_RETRY_S) * 1000;
				return {
					ctx: {
						...ctx,
						lockedUntil: retryAt,
						state: { kind: 'confirm', change, assessment, error: 'rate_limited', retryAt }
					},
					effects: []
				};
			}
			if (e.status === 409 && e.code === 'changed_since_preview') {
				return preview(ctx, change, event.now);
			}
			if (e.status === 401 && e.code === 'login_required') return closed(ctx);
			if (e.code === 'floor_removal_needs_hashes') {
				return failed(ctx, e, [
					{
						type: 'dev_error',
						message:
							'filters flow: apply sent a floor removal without the assessment’s floor_entries'
					}
				]);
			}
			return failed(ctx, e);
		}
	}
}
