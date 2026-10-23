// The change flow as runes: holds the machine's context and runs its effects through W5's client.
// One change at a time. See machine.ts for the table; nothing here decides anything about a change.
import { api, isAbort } from '$lib/api/client';
import {
	initialCtx,
	transition,
	type Ctx,
	type Effect,
	type Event,
	type FlowState
} from './machine';
import type { Change } from './types';

export interface ChangeFlow {
	readonly state: FlowState;
	/** Previews `change` (always, as a background job) and then waits for the user. */
	start(change: Change): void;
	/** In `review` (the server asked for no password): applies, sending no password. */
	apply(): void;
	/** In `confirm`: applies with the password. The only way a password leaves the dialog. */
	confirm(password: string): void;
	/** In `review` or `confirm`: previews a variant (shadow days, the CSX-5 strength) instead. */
	variant(change: Change): void;
	/** In `failed`: previews the same change again. */
	retry(): void;
	/** Any state → `closed`. Cancels a running preview job; a late answer never changes state. */
	cancel(): void;
}

export interface FlowOptions {
	/** Injectable for tests. */
	now?: () => number;
}

export function createChangeFlow({ now = Date.now }: FlowOptions = {}): ChangeFlow {
	let ctx = $state.raw<Ctx>(initialCtx);
	// Polls of the current preview; aborted when it is superseded, cancelled or finished.
	let polls = new AbortController();
	let timer: ReturnType<typeof setTimeout> | undefined;

	function send(event: Event) {
		const step = transition(ctx, event);
		ctx = step.ctx;
		for (const effect of step.effects) run(effect);
	}

	function run(effect: Effect) {
		switch (effect.type) {
			case 'abort':
				clearTimeout(timer);
				timer = undefined;
				polls.abort();
				polls = new AbortController();
				return;
			case 'start_job': {
				const { seq, change } = effect;
				// Never aborted: a job the server started must be named so it can be cancelled.
				api.post('/filters/preview/jobs', { body: { change } }).then(
					(r) => send({ type: 'job_started', seq, job: r.job }),
					(error) => send({ type: 'job_error', seq, error, phase: 'start', now: now() })
				);
				return;
			}
			case 'poll': {
				const { seq, job, delay } = effect;
				const signal = polls.signal;
				clearTimeout(timer);
				timer = setTimeout(() => {
					timer = undefined;
					api.get('/filters/preview/jobs/{id}', { path: { id: job }, signal }).then(
						(status) => !signal.aborted && send({ type: 'job_status', seq, status, now: now() }),
						(error) =>
							!signal.aborted &&
							!isAbort(error) &&
							send({ type: 'job_error', seq, error, phase: 'poll', now: now() })
					);
				}, delay);
				return;
			}
			case 'cancel_job':
				api
					.post('/filters/preview/jobs/{id}/cancel', { path: { id: effect.job }, body: {} })
					.catch(() => {});
				return;
			case 'apply': {
				const { seq } = effect;
				api.post('/filters/apply', { body: effect.body }).then(
					(response) => send({ type: 'applied', seq, response }),
					(error) => send({ type: 'apply_error', seq, error, now: now() })
				);
				return;
			}
			case 'dev_error':
				if (import.meta.env.DEV) console.error(effect.message);
				return;
		}
	}

	return {
		get state() {
			return ctx.state;
		},
		start: (change) => send({ type: 'start', change, now: now() }),
		apply: () => send({ type: 'apply' }),
		confirm: (password) => send({ type: 'confirm', password, now: now() }),
		variant: (change) => send({ type: 'variant', change, now: now() }),
		retry: () => send({ type: 'retry', now: now() }),
		cancel: () => send({ type: 'cancel' })
	};
}
