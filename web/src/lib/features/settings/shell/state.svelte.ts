// What the Settings layout shares with its pages: the schema index, the registry resolved from
// it, `/system` (restart, address change, what's changed), the Overview's attention items and the
// navigation's badges, and whether the two panes fit (x-large, §10.10.2). The layout creates it;
// pages read it. Attention and its providers load lazily, once `/system` and the schema are in.
import { getContext, setContext } from 'svelte';
import { MediaQuery } from 'svelte/reactivity';
import { api, isAbort } from '$lib/api/client';
import { loadSettingMessages } from '../form/messages';
import type { SettingsIndexView } from '../form/schema';
import { loadSettingsIndex } from '../form/schema';
import { buildBadges, type Badge } from './badges';
import { buildRegistry, type AttentionItem, type Registry } from './registry';
import type { SystemInfo } from './system';

const KEY = Symbol('settings');

/** x-large and wider (`--breakpoint-x-large`, 1024 px): navigation and screen side by side. */
const WIDE = '(min-width: 64rem)';

type Status = 'loading' | 'ready' | 'failed';

export interface SettingsState {
	readonly status: Status;
	readonly index: SettingsIndexView | undefined;
	readonly registry: Registry;
	/** Two panes (x-large and wider); below it the list and a screen take turns. */
	readonly wide: boolean;
	retry(): void;
	/** `GET /system`, fetched once by the layout and refreshed on `settings` events. */
	readonly system: SystemInfo | undefined;
	readonly systemStatus: Status;
	refreshSystem(): Promise<void>;
	/** The Overview's rows; `ready` once every provider has answered. */
	readonly attention: { ready: boolean; items: AttentionItem[]; partial: boolean };
	readonly badges: Record<string, Badge>;
}

export function createSettingsState(): SettingsState {
	let status = $state<Status>('loading');
	let index = $state.raw<SettingsIndexView>();
	const registry = $derived(buildRegistry(index));
	let system = $state.raw<SystemInfo>();
	let systemStatus = $state<Status>('loading');
	let attention = $state.raw({ ready: false, items: [] as AttentionItem[], partial: false });
	// A MediaQuery is reactive in a component; without `matchMedia` (a test) the panes stack.
	const media = typeof matchMedia === 'function' ? new MediaQuery(WIDE) : undefined;

	let seq = 0;
	let ctl: AbortController | undefined;
	/** Runs the providers against the latest `/system` and schema; a newer run supersedes this one. */
	async function recompute() {
		if (!system || !index) return;
		const mine = ++seq;
		ctl?.abort();
		const c = (ctl = new AbortController());
		try {
			const { collectAttention } = await import('./attention');
			const result = await collectAttention({ system, registry, signal: c.signal });
			if (mine === seq) attention = { ready: true, ...result };
		} catch (e) {
			if (!isAbort(e) && mine === seq) attention = { ready: true, items: [], partial: true };
		}
	}

	function load() {
		status = 'loading';
		// The group headings and each section's label (an interim screen's title) come with it.
		Promise.all([loadSettingsIndex(), loadSettingMessages([])]).then(
			([i]) => {
				index = i;
				status = 'ready';
				void recompute();
			},
			() => (status = 'failed')
		);
	}

	async function refreshSystem() {
		try {
			system = await api.get('/system');
			systemStatus = 'ready';
			void recompute();
		} catch (e) {
			// A refresh that fails keeps what was shown; only the first load is a failure.
			if (!isAbort(e) && !system) systemStatus = 'failed';
		}
	}
	function retry() {
		if (status === 'failed') load();
		if (systemStatus === 'failed') {
			systemStatus = 'loading';
			void refreshSystem();
		}
	}
	load();
	void refreshSystem();

	const badges = $derived(
		attention.ready || system
			? buildBadges(attention.items, system?.restart_required ?? [], registry)
			: {}
	);

	return {
		get status() {
			return status;
		},
		get index() {
			return index;
		},
		get registry() {
			return registry;
		},
		get wide() {
			return media?.current ?? false;
		},
		retry,
		get system() {
			return system;
		},
		get systemStatus() {
			return systemStatus;
		},
		refreshSystem,
		get attention() {
			return attention;
		},
		get badges() {
			return badges;
		}
	};
}

export function provideSettings(state: SettingsState): SettingsState {
	return setContext(KEY, state);
}

export function useSettings(): SettingsState {
	const state = getContext<SettingsState | undefined>(KEY);
	if (!state) throw new Error('useSettings() outside the Settings layout');
	return state;
}
