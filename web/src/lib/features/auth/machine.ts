// The auth forms' state machine: explicit, finite and pure, so every transition is unit-tested.
//
//   idle ──submit──▶ submitting ──ok──▶ success
//    ▲  ╲               │
//    │   invalid        ├──fail(kind)──────▶ error ──edit──▶ idle
//    │     ╲            │                     │  ╲
//    │      ▶ error     └──fail(rate_limited)─┼───▶ locked ──tick(now ≥ until)──▶ idle
//    │                                        └──submit──▶ submitting
//    └────────────────────────────────────────────────────────────────────────┘
//
// Anything else is ignored: a second submit while submitting (no double submit, no burnt rate-
// limit tokens), a submit while locked, every event after success.
import { ApiError } from '$lib/api/client';

/** Server answers the forms tell apart (crates/api/src/auth/http.rs), plus the client's own. */
export type FailKind =
	| 'bad_token'
	| 'bad_password'
	| 'weak_password'
	| 'setup_done'
	| 'not_set_up'
	| 'rate_limited'
	| 'network'
	| 'server'
	/** Client-side validation: the request was never sent. */
	| 'invalid';

/** The form field an error belongs to, for focus and `aria-describedby`. */
export type FieldName = 'token' | 'password' | 'confirm';

export interface Failure {
	kind: FailKind;
	field?: FieldName;
	/** What exactly is wrong when `kind` is `invalid` (a message key suffix). */
	reason?: string;
	/** Rate limited until this time (ms since the epoch). */
	until?: number;
}

export type State =
	| { name: 'idle' }
	| { name: 'submitting' }
	| { name: 'success' }
	| { name: 'error'; failure: Failure }
	| { name: 'locked'; until: number };

export type Event =
	| { type: 'submit' }
	| { type: 'invalid'; failure: Failure }
	| { type: 'ok' }
	| { type: 'fail'; failure: Failure }
	| { type: 'edit' }
	| { type: 'tick'; now: number };

export const initial: State = { name: 'idle' };

/** The server limits sign-in to one attempt every 12 s after a burst of five; it sends no
 * `Retry-After`, so this is the wait when the header is missing. */
export const DEFAULT_RETRY_SECONDS = 12;
/** A `Retry-After` beyond this is capped, so a bad header can't lock the form for hours. */
export const MAX_RETRY_SECONDS = 15 * 60;

export function transition(state: State, event: Event): State {
	switch (state.name) {
		case 'idle':
		case 'error':
			if (event.type === 'submit') return { name: 'submitting' };
			if (event.type === 'invalid') return { name: 'error', failure: event.failure };
			if (event.type === 'edit' && state.name === 'error') return initial;
			return state;
		case 'submitting':
			if (event.type === 'ok') return { name: 'success' };
			if (event.type === 'fail') {
				const { failure } = event;
				return failure.kind === 'rate_limited' && failure.until !== undefined
					? { name: 'locked', until: failure.until }
					: { name: 'error', failure };
			}
			return state;
		case 'locked':
			return event.type === 'tick' && event.now >= state.until ? initial : state;
		case 'success':
			return state;
	}
}

/** Whether the form may send now. */
export const canSubmit = (s: State) => s.name === 'idle' || s.name === 'error';

/** Whole seconds left on a lock, at least 1 while it lasts. */
export const secondsLeft = (s: State, now: number) =>
	s.name === 'locked' ? Math.max(0, Math.ceil((s.until - now) / 1000)) : 0;

/**
 * Maps whatever the request threw to a `Failure`. Only the code and status are used, never the
 * server's message, so nothing the user typed (the token above all) is ever echoed back.
 */
export function classify(e: unknown, now: number): Failure {
	if (!(e instanceof ApiError)) return { kind: 'server' };
	switch (e.code) {
		case 'bad_token':
			return { kind: 'bad_token', field: 'token' };
		case 'bad_password':
			return { kind: 'bad_password', field: 'password' };
		case 'weak_password':
			return { kind: 'weak_password', field: 'password' };
		case 'setup_done':
			return { kind: 'setup_done' };
		case 'not_set_up':
			return { kind: 'not_set_up' };
		case 'network':
			return { kind: 'network' };
	}
	if (e.status === 429) {
		const wait = Math.min(e.retryAfter ?? DEFAULT_RETRY_SECONDS, MAX_RETRY_SECONDS);
		return { kind: 'rate_limited', until: now + Math.max(1, wait) * 1000 };
	}
	return { kind: 'server' };
}
