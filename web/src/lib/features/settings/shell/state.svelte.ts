// What the Settings layout shares with its pages: the schema index, the registry resolved from
// it, and whether the two panes fit (x-large, §10.10.2). The layout creates it; pages read it.
import { getContext, setContext } from 'svelte';
import { MediaQuery } from 'svelte/reactivity';
import type { SettingsIndexView } from '../form/schema';
import { loadSettingsIndex } from '../form/schema';
import { loadSettingMessages } from '../form/messages';
import { buildRegistry, type Registry } from './registry';

const KEY = Symbol('settings');

/** x-large and wider (`--breakpoint-x-large`, 1024 px): navigation and screen side by side. */
const WIDE = '(min-width: 64rem)';

export interface SettingsState {
	readonly status: 'loading' | 'ready' | 'failed';
	readonly index: SettingsIndexView | undefined;
	readonly registry: Registry;
	/** Two panes (x-large and wider); below it the list and a screen take turns. */
	readonly wide: boolean;
	retry(): void;
}

export function createSettingsState(): SettingsState {
	let status = $state<'loading' | 'ready' | 'failed'>('loading');
	let index = $state.raw<SettingsIndexView>();
	const registry = $derived(buildRegistry(index));
	// A MediaQuery is reactive in a component; without `matchMedia` (a test) the panes stack.
	const media = typeof matchMedia === 'function' ? new MediaQuery(WIDE) : undefined;

	function load() {
		status = 'loading';
		// The group headings and each section's label (an interim screen's title) come with it.
		Promise.all([loadSettingsIndex(), loadSettingMessages([])]).then(
			([i]) => {
				index = i;
				status = 'ready';
			},
			() => (status = 'failed')
		);
	}
	load();

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
		retry: load
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
