// Every built-in control but the toggle and the text input loads per type: the first time a
// field of that type renders, or as soon as a form knows its fields (`SettingsForm.svelte`
// starts them while the values load). Until then `LazyControl.svelte` holds their space.
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
	/** The control `widgetFor` picked (one file serves segmented, radio and select). */
	builtin: BuiltinWidget;
	dense?: boolean;
}

const LOADERS: Record<string, () => Promise<{ default: Component<LazyControlProps> }>> = {
	number: () => import('./NumberControl.svelte'),
	slider: () => import('./SliderControl.svelte'),
	choice: () => import('./ChoiceControl.svelte'),
	secret: () => import('./SecretControl.svelte'),
	chips: () => import('./ChipsControl.svelte'),
	json: () => import('./JsonControl.svelte')
};
export type LazyWidget = 'number' | 'slider' | 'choice' | 'secret' | 'chips' | 'json';

const loaded = $state<Partial<Record<LazyWidget, Component<LazyControlProps>>>>({});
const failed = $state<Partial<Record<LazyWidget, true>>>({});
const loading = new Map<LazyWidget, Promise<void>>();

/** The file that renders a built-in control; undefined for the toggle and the text input. */
export function lazyOf(w: BuiltinWidget): LazyWidget | undefined {
	if (w === 'segmented' || w === 'radio' || w === 'select') return 'choice';
	return Object.hasOwn(LOADERS, w) ? (w as LazyWidget) : undefined;
}

/** Starts loading a control, once. */
export function loadControl(w: LazyWidget): Promise<void> {
	let p = loading.get(w);
	if (!p) {
		p = LOADERS[w]!().then(
			(mod) => {
				loaded[w] = mod.default;
			},
			() => {
				// Browsers keep a failed module: only a reload loads it now.
				failed[w] = true;
			}
		);
		loading.set(w, p);
	}
	return p;
}

/** The control, once loaded (reactive). */
export const controlOf = (w: LazyWidget) => loaded[w];

/** Its code didn't load (reactive). */
export const controlFailed = (w: LazyWidget) => failed[w] === true;

/** Starts loading the controls these widget choices need. */
export function loadControls(choices: Iterable<WidgetChoice>): Promise<void> {
	const wanted = new Set<LazyWidget>();
	for (const c of choices) {
		const w = 'builtin' in c ? lazyOf(c.builtin) : undefined;
		if (w) wanted.add(w);
	}
	return Promise.all([...wanted].map(loadControl)).then(() => {});
}
