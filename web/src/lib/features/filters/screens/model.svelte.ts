// State of the Filters & Safety screen. Everything on screen is the server's last answer: a switch
// is never flipped locally, a change starts a flow and the screen reloads `GET /filters` once the
// server says it applied. A failed or refused request therefore leaves what was there (the safer
// state, unless the server itself said otherwise).
import { api, isAbort } from '$lib/api/client';
import type { components } from '$lib/api/schema';
import type { ChangeFlow } from '../flows/flow.svelte';
import type { Change } from '../flows/types';

export type Overview = components['schemas']['Overview'];
export type Queue = components['schemas']['Queue'];

export interface Pending {
	title: string;
	applyLabel?: string;
	shadowDays?: number;
}

/** Whether "Show adult content" is on. Anything but a clear `true` reads as off. */
export function adultOn(res: unknown): boolean {
	const f = (res as { fields?: Record<string, { value?: unknown }> } | null)?.fields?.show_adult;
	return f?.value === true;
}

export function createModel() {
	let overview = $state.raw<Overview | null>(null);
	let failed = $state(false);
	let adult = $state(false);
	let pending = $state.raw<Pending>({ title: '' });
	let destroyed = false;
	let creating: Promise<ChangeFlow> | undefined;
	// The flow's module is shared with other screens' dialogs, so it loads with the first change.
	let flow = $state.raw<ChangeFlow | null>(null);
	let ctl: AbortController | undefined;

	async function load() {
		ctl?.abort();
		const mine = (ctl = new AbortController());
		try {
			const [o, s] = await Promise.all([
				api.get('/filters', { signal: mine.signal }),
				// Unreadable → off: nothing adult shows unless the setting was read as on.
				api
					.get('/settings/{section}', { path: { section: 'content' }, signal: mine.signal })
					.catch((e) => {
						if (isAbort(e)) throw e;
						return null;
					})
			]);
			overview = o;
			adult = adultOn(s);
			failed = false;
		} catch (e) {
			if (!isAbort(e)) failed = true;
		}
	}

	/** Previews `change` through the flow's dialog; nothing is sent until the dialog says so. */
	async function start(change: Change, p: Pending) {
		// One flow, however many activations arrive while its module loads.
		creating ??= import('../flows-door')
			.then((door) => door.load())
			.then((m) => m.createChangeFlow());
		const f = await creating;
		// A screen that went away meanwhile starts nothing (its preview job would never be cancelled).
		if (destroyed) return;
		flow = f;
		// A change already applying keeps its own title and Apply label.
		if (f.state.kind !== 'applying') pending = p;
		f.start(change);
	}

	return {
		get flow() {
			return flow;
		},
		get overview() {
			return overview;
		},
		get failed() {
			return failed;
		},
		get adult() {
			return adult;
		},
		get pending() {
			return pending;
		},
		load,
		start,
		destroy() {
			destroyed = true;
			ctl?.abort();
			// An apply already sent can't be cancelled: let its answer go (flows README).
			if (flow && flow.state.kind !== 'applying') flow.cancel();
		}
	};
}
export type Model = ReturnType<typeof createModel>;
