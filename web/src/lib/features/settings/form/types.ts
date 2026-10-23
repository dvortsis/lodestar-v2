// The form engine's own types. Everything the server sends uses the generated types
// (`$lib/api/schema`); these describe what the engine derives from them.
import type { components } from '$lib/api/schema';

export type SettingSource = components['schemas']['SettingSource'];
export type SettingValue = components['schemas']['SettingValue'];
export type SectionValues = components['schemas']['SectionValues'];
export type PreviewEstimate = components['schemas']['PreviewEstimate'];
export type PreviewWarning = components['schemas']['PreviewWarning'];
export type SettingsPreview = components['schemas']['SettingsPreview'];
export type SettingsEvent = components['schemas']['SettingsEvent'];

/** `${section}.${field}`, as the URL hash and `/system`'s `restart_required` name it. */
export type FieldKey = `${string}.${string}`;

export type FieldKind =
	'boolean' | 'enum' | 'integer' | 'number' | 'string' | 'string-list' | 'map' | 'unknown';

export type Danger = 'disruptive' | 'privacy' | 'destructive' | 'security';

/** One field, derived from its section's JSON Schema property and its `SettingField`. */
export interface FieldSpec {
	key: FieldKey;
	section: string;
	name: string;
	/** Server message keys, as sent (`settings.crawler.intensity.label`). */
	labelKey: string;
	helpKey: string;
	keywordsKey?: string;
	kind: FieldKind;
	enum?: readonly string[];
	minimum?: number;
	maximum?: number;
	format?: string;
	unit?: string;
	widget?: string;
	/** The value that means "automatic" (`x-auto`); absent when the field has none. */
	auto?: unknown;
	preview: boolean;
	danger?: Danger;
	group: string;
	order: number;
	advanced: boolean;
	config?: string;
	env?: string;
	writeOnly: boolean;
	readOnly: boolean;
	restartRequired: boolean;
}

/** One field as a screen renders it. */
export interface FieldState {
	spec: FieldSpec;
	/** What the control shows: the staged value, else the saved one (null for a secret). */
	value: unknown;
	saved: unknown;
	source: SettingSource;
	env: string | null;
	/** `read_only`, or set by the environment: no edits, no reset. */
	locked: boolean;
	dirty: boolean;
	/** `source !== 'default'`: the schema has no defaults to compare with. */
	changed: boolean;
	/** A staged "remove the saved value" (B2's `null` patch). */
	pendingReset: boolean;
	error?: string;
	/** Something the engine did to this field that the user should know (an edit dropped). */
	note?: string;
	conflict?: { mine: unknown; theirs: unknown };
	preview?: { restartRequired: boolean; estimates: PreviewEstimate[]; warnings: PreviewWarning[] };
	/** Saved by the last save, while its Undo is offered. */
	savedRecently?: 'live' | 'restart';
	/** Instant mode: while this field's own Undo is offered (ms timestamp). */
	undoUntil?: number;
	/** A secret with a saved value (`source !== 'default'`, the value never sent). */
	secretSet: boolean;
}

export type SaveResult =
	| { ok: true; restart: FieldKey[] }
	| {
			ok: false;
			reason: 'nothing' | 'invalid' | 'conflict' | 'cancelled' | 'network' | 'refused' | 'busy';
	  };

export interface GateContext {
	fields: FieldKey[];
	preview: SettingsPreview;
}

export type ConfirmHandler = (c: {
	section: string;
	field?: string;
	message: string;
}) => Promise<Record<string, unknown> | null>;

export type GateHandler = (c: GateContext) => Promise<boolean>;

export type FormStatus =
	'loading' | 'load-error' | 'ready' | 'saving' | 'saved' | 'save-error' | 'offline';

export function splitKey(key: string): [section: string, field: string] {
	const i = key.indexOf('.');
	return [key.slice(0, i), key.slice(i + 1)];
}
