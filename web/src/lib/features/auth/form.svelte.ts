// Runs the state machine for one form: holds the state as a rune, sends the request, and ticks
// once a second while rate limited so the countdown moves and the form unlocks on its own.
import {
	canSubmit,
	classify,
	initial,
	secondsLeft,
	transition,
	type Event,
	type Failure
} from './machine';

export interface FormOptions {
	/** Injectable for tests. */
	now?: () => number;
}

export function createAuthForm({ now = Date.now }: FormOptions = {}) {
	let state = $state(initial);
	let clock = $state(now());
	let timer: ReturnType<typeof setInterval> | undefined;

	function stopTimer() {
		if (timer !== undefined) clearInterval(timer);
		timer = undefined;
	}

	function send(event: Event) {
		state = transition(state, event);
		if (state.name === 'locked' && timer === undefined) {
			clock = now();
			timer = setInterval(() => {
				clock = now();
				send({ type: 'tick', now: clock });
			}, 1000);
		} else if (state.name !== 'locked') {
			stopTimer();
		}
	}

	return {
		get state() {
			return state;
		},
		/** Whole seconds until a rate-limited form unlocks; 0 when it isn't locked. */
		get secondsLeft() {
			return secondsLeft(state, clock);
		},
		get busy() {
			return state.name === 'submitting';
		},
		/** Client-side validation failed; nothing is sent. */
		invalid(failure: Failure) {
			send({ type: 'invalid', failure });
		},
		/** The user changed a field: a shown error no longer applies. */
		edit() {
			send({ type: 'edit' });
		},
		/**
		 * Sends `request` unless the form is already sending, locked or done. Resolves true on
		 * success, false otherwise (including when it was ignored).
		 */
		async submit(request: () => Promise<unknown>): Promise<boolean> {
			if (!canSubmit(state)) return false;
			send({ type: 'submit' });
			try {
				await request();
			} catch (e) {
				send({ type: 'fail', failure: classify(e, now()) });
				return false;
			}
			send({ type: 'ok' });
			return true;
		},
		/** Stops the countdown timer (call on unmount). */
		destroy: stopTimer
	};
}

export type AuthForm = ReturnType<typeof createAuthForm>;
