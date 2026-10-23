// The widget registry (§10.10.4's table): one function picks every field's control. Features
// register their own widgets by `x-widget` name (S-logs `log-events`, S-appearance `theme`, …).
import type { Component } from 'svelte';
import type { FieldControlProps } from '$lib/ui/fields/Field.svelte';
import { devWarn } from './schema';
import type { SettingsForm } from './form.svelte';
import type { FieldKey, FieldSpec } from './types';

/** The controls the engine ships. `json` is the fallback that keeps a new setting visible. */
export type BuiltinWidget =
	| 'toggle'
	| 'segmented'
	| 'radio'
	| 'select'
	| 'number'
	| 'slider'
	| 'text'
	| 'chips'
	| 'secret'
	| 'json';

export type { FieldControlProps };

export interface WidgetProps {
	form: SettingsForm;
	key: FieldKey;
	controlProps: FieldControlProps;
	dense?: boolean;
}

export type WidgetChoice =
	{ builtin: BuiltinWidget; mono: boolean } | { name: string; component: Component<WidgetProps> };

/** Enums with more values than this get a Select instead of a SegmentedControl. */
export const SEGMENTED_MAX = 4;

/** `x-widget` names that pick a built-in control rather than a registered one. */
const HINTS: Record<string, BuiltinWidget> = { radio: 'radio', slider: 'slider' };

/** Formats shown in Mono: addresses and log filters. */
const MONO_FORMATS = new Set(['socket-addr', 'host', 'url', 'log-filter']);

const registry = new Map<string, Component<WidgetProps>>();
const warned = new Set<string>();

function warnOnce(key: string, message: string) {
	if (warned.has(key)) return;
	warned.add(key);
	devWarn(message);
}

/**
 * Registers a control for fields whose `x-widget` is `name`. Returns the unregister. A later
 * registration under the same name replaces the earlier one until it's unregistered.
 */
export function registerWidget(name: string, component: Component<WidgetProps>): () => void {
	registry.set(name, component);
	return () => {
		if (registry.get(name) === component) registry.delete(name);
	};
}

function shapeDefault(spec: FieldSpec): BuiltinWidget {
	switch (spec.kind) {
		case 'boolean':
			return 'toggle';
		case 'enum':
			return (spec.enum?.length ?? 0) <= SEGMENTED_MAX ? 'segmented' : 'select';
		case 'integer':
		case 'number':
			return 'number';
		case 'string':
			return 'text';
		case 'string-list':
			return 'chips';
		default:
			return 'json';
	}
}

/** The control for a field: a registered `x-widget` wins, then the schema's shape. */
export function widgetFor(spec: FieldSpec): WidgetChoice {
	const mono = spec.format !== undefined && MONO_FORMATS.has(spec.format);
	if (spec.writeOnly) return { builtin: 'secret', mono: false };
	const w = spec.widget;
	if (w !== undefined) {
		const component = registry.get(w);
		if (component) return { name: w, component };
		const hint = HINTS[w];
		if (hint === 'radio' && spec.kind === 'enum') return { builtin: 'radio', mono };
		if (
			hint === 'slider' &&
			(spec.kind === 'integer' || spec.kind === 'number') &&
			spec.minimum !== undefined &&
			spec.maximum !== undefined
		) {
			return { builtin: 'slider', mono };
		}
		if (import.meta.env.DEV)
			warnOnce(
				`${spec.key}:${w}`,
				`${spec.key}: no widget "${w}" is registered; using the default`
			);
	}
	const builtin = shapeDefault(spec);
	if (import.meta.env.DEV && builtin === 'json')
		warnOnce(spec.key, `${spec.key} falls back to the JSON input`);
	return { builtin, mono };
}
