import { describe, expect, it } from 'vitest';
import { ApiError } from '$lib/api/client';
import {
	canSubmit,
	classify,
	DEFAULT_RETRY_SECONDS,
	initial,
	MAX_RETRY_SECONDS,
	secondsLeft,
	transition,
	type Event,
	type Failure,
	type State
} from './machine';

const idle: State = initial;
const submitting: State = { name: 'submitting' };
const success: State = { name: 'success' };
const bad: Failure = { kind: 'bad_password', field: 'password' };
const error: State = { name: 'error', failure: bad };
const locked: State = { name: 'locked', until: 10_000 };

const submit: Event = { type: 'submit' };
const ok: Event = { type: 'ok' };
const edit: Event = { type: 'edit' };
const invalid: Event = { type: 'invalid', failure: { kind: 'invalid', field: 'password' } };
const fail: Event = { type: 'fail', failure: bad };
const limited: Event = { type: 'fail', failure: { kind: 'rate_limited', until: 10_000 } };
const tickEarly: Event = { type: 'tick', now: 9_999 };
const tickLate: Event = { type: 'tick', now: 10_000 };

const ALL: Event[] = [submit, ok, edit, invalid, fail, limited, tickEarly, tickLate];

describe('transition', () => {
	// The whole table: [state, event, expected]. Every state × event pair is listed.
	const table: [string, State, Event, State][] = [
		['idle', idle, submit, submitting],
		['idle', idle, invalid, { name: 'error', failure: { kind: 'invalid', field: 'password' } }],
		['idle', idle, ok, idle],
		['idle', idle, edit, idle],
		['idle', idle, fail, idle],
		['idle', idle, limited, idle],
		['idle', idle, tickEarly, idle],
		['idle', idle, tickLate, idle],

		['submitting', submitting, ok, success],
		['submitting', submitting, fail, error],
		['submitting', submitting, limited, locked],
		['submitting', submitting, submit, submitting],
		['submitting', submitting, invalid, submitting],
		['submitting', submitting, edit, submitting],
		['submitting', submitting, tickEarly, submitting],
		['submitting', submitting, tickLate, submitting],

		['error', error, submit, submitting],
		['error', error, edit, idle],
		['error', error, invalid, { name: 'error', failure: { kind: 'invalid', field: 'password' } }],
		['error', error, ok, error],
		['error', error, fail, error],
		['error', error, limited, error],
		['error', error, tickEarly, error],
		['error', error, tickLate, error],

		['locked', locked, tickEarly, locked],
		['locked', locked, tickLate, idle],
		['locked', locked, submit, locked],
		['locked', locked, edit, locked],
		['locked', locked, invalid, locked],
		['locked', locked, ok, locked],
		['locked', locked, fail, locked],
		['locked', locked, limited, locked]
	];

	it.each(table)('%s + %o', (_, state, event, want) => {
		expect(transition(state, event)).toEqual(want);
	});

	it('covers every state × event pair', () => {
		for (const s of [idle, submitting, error, locked]) {
			for (const e of ALL) expect(table.some(([, ts, te]) => ts === s && te === e)).toBe(true);
		}
	});

	it('success is final', () => {
		for (const e of ALL) expect(transition(success, e)).toBe(success);
	});

	it('a rate limit without a time is an error, never a lock that cannot end', () => {
		expect(transition(submitting, { type: 'fail', failure: { kind: 'rate_limited' } })).toEqual({
			name: 'error',
			failure: { kind: 'rate_limited' }
		});
	});
});

describe('canSubmit and secondsLeft', () => {
	it('allows sending only from idle and error', () => {
		expect([idle, submitting, success, error, locked].map(canSubmit)).toEqual([
			true,
			false,
			false,
			true,
			false
		]);
	});

	it('rounds the wait up to whole seconds', () => {
		expect(secondsLeft(locked, 0)).toBe(10);
		expect(secondsLeft(locked, 8_001)).toBe(2);
		expect(secondsLeft(locked, 9_999)).toBe(1);
		expect(secondsLeft(locked, 10_000)).toBe(0);
		expect(secondsLeft(idle, 0)).toBe(0);
	});
});

describe('classify', () => {
	const err = (status: number, code: string, retryAfter?: number) =>
		new ApiError(status, code, 'server text with AB12-CD34-EF56-7890', { retryAfter });

	it.each([
		[err(401, 'bad_token'), { kind: 'bad_token', field: 'token' }],
		[err(401, 'bad_password'), { kind: 'bad_password', field: 'password' }],
		[err(400, 'weak_password'), { kind: 'weak_password', field: 'password' }],
		[err(409, 'setup_done'), { kind: 'setup_done' }],
		[err(409, 'not_set_up'), { kind: 'not_set_up' }],
		[err(0, 'network'), { kind: 'network' }],
		[err(500, 'internal'), { kind: 'server' }],
		[err(403, 'cross_origin'), { kind: 'server' }],
		[err(502, 'http_502'), { kind: 'server' }],
		[new TypeError('boom'), { kind: 'server' }]
	])('%o', (e, want) => {
		expect(classify(e, 0)).toEqual(want);
	});

	it('never carries the server message (nothing typed is echoed)', () => {
		expect(JSON.stringify(classify(err(401, 'bad_token'), 0))).not.toContain('AB12');
	});

	it('locks for Retry-After, or the server refill interval without one', () => {
		expect(classify(err(429, 'rate_limited', 30), 1_000)).toEqual({
			kind: 'rate_limited',
			until: 31_000
		});
		expect(classify(err(429, 'rate_limited'), 0).until).toBe(DEFAULT_RETRY_SECONDS * 1000);
		expect(classify(err(429, 'rate_limited', 0), 0).until).toBe(1000);
		expect(classify(err(429, 'rate_limited', 86_400), 0).until).toBe(MAX_RETRY_SECONDS * 1000);
	});
});
