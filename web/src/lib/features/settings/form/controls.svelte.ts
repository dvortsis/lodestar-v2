// The built-in controls a section's first view rarely needs (secrets, chip lists, the JSON
// fallback) load per type, the first time a field of that type renders or a form lists one.
// Until then `LazyControl.svelte` holds their space.
// The Map and Set are bookkeeping nothing renders; `loaded` is the reactive state.
/* eslint-disable svelte/prefer-svelte-reactivity */
import type { Component } from 'svelte';
import type { SettingsForm } from './form.svelte';
import type { FieldKey } from './types';
import type { BuiltinWidget, FieldControlProps, WidgetChoice } from './widgets';

/** What `LazyControl` passes; each control takes the part it uses. */
export interface LazyControlProps {
	form: SettingsForm;
	key: FieldKey;
	controlProps: FieldControlProps;
	label: string;
	dense?: boolean;
}

const LOADERS: Record<string, () => Promise<{ default: Component<LazyControlProps> }>> = {
	secret: () => import('./SecretControl.svelte'),
	chips: () => import('./ChipsControl.svelte'),
	json: () => import('./JsonControl.svelte')
};
export type LazyWidget = 'secret' | 'chips' | 'json';

const loaded = $state<Partial<Record<LazyWidget, Component<LazyControlProps>>>>({});
const loading = new Map<LazyWidget, Promise<void>>();

export const isLazy = (w: BuiltinWidget): w is LazyWidget => Object.hasOwn(LOADERS, w);

/** Starts loading a control (once; a failed load is tried again next time). */
export function loadControl(w: LazyWidget): Promise<void> {
	let p = loading.get(w);
	if (!p) {
		p = LOADERS[w]!().then(
			(mod) => {
				loaded[w] = mod.default;
			},
			() => {
				loading.delete(w);
			}
		);
		loading.set(w, p);
	}
	return p;
}

/** The control, once loaded (reactive). */
export const controlOf = (w: LazyWidget) => loaded[w];

/** Starts loading the controls these widget choices need. */
export function loadControls(choices: Iterable<WidgetChoice>): Promise<void> {
	const wanted = new Set<LazyWidget>();
	for (const c of choices) if ('builtin' in c && isLazy(c.builtin)) wanted.add(c.builtin);
	return Promise.all([...wanted].map(loadControl)).then(() => {});
}
