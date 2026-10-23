// Reads `GET /settings` into `FieldSpec`s. A section's `schema` is the server's JSON Schema,
// typed as an untyped object: every property is narrowed here before use, and a shape this
// reader doesn't know becomes `kind: 'unknown'` (the JSON fallback) with a dev warning.
import type { components } from '$lib/api/schema';
import { getIndex } from './requests';
import type { Danger, FieldKey, FieldKind, FieldSpec } from './types';

type SettingsIndex = components['schemas']['SettingsIndex'];
type SettingsSection = components['schemas']['SettingsSection'];

const DANGERS: readonly string[] = ['disruptive', 'privacy', 'destructive', 'security'];

/** Dev-only warnings: a schema shape, widget, unit or message the UI doesn't know yet. */
export function devWarn(message: string): void {
	if (import.meta.env.DEV) console.warn(`settings: ${message}`);
}

type Obj = Record<string, unknown>;
const isObj = (v: unknown): v is Obj => typeof v === 'object' && v !== null && !Array.isArray(v);
const str = (v: unknown): string | undefined => (typeof v === 'string' ? v : undefined);
const num = (v: unknown): number | undefined =>
	typeof v === 'number' && Number.isFinite(v) ? v : undefined;

/** The one non-null `type` of a property (`["string", "null"]` reads as `string`). */
function typeOf(p: Obj): string | undefined {
	const t = p.type;
	if (typeof t === 'string') return t;
	if (Array.isArray(t)) {
		const named = t.filter((x): x is string => typeof x === 'string' && x !== 'null');
		return named.length === 1 ? named[0] : undefined;
	}
	return undefined;
}

function kindOf(p: Obj): { kind: FieldKind; enum?: string[] } {
	const type = typeOf(p);
	if (Array.isArray(p.enum)) {
		const values = p.enum;
		if (type === 'string' && values.length > 0 && values.every((v) => typeof v === 'string')) {
			return { kind: 'enum', enum: values as string[] };
		}
		return { kind: 'unknown' };
	}
	switch (type) {
		case 'boolean':
			return { kind: 'boolean' };
		case 'integer':
			return { kind: 'integer' };
		case 'number':
			return { kind: 'number' };
		case 'string':
			return { kind: 'string' };
		case 'array':
			return isObj(p.items) && typeOf(p.items) === 'string' && !Array.isArray(p.items.enum)
				? { kind: 'string-list' }
				: { kind: 'unknown' };
		case 'object':
			// A map: string keys, values of one schema. Fixed `properties` aren't a map.
			return isObj(p.additionalProperties) && p.properties === undefined
				? { kind: 'map' }
				: { kind: 'unknown' };
		default:
			return { kind: 'unknown' };
	}
}

/** One section's fields, in the order the schema gives (`x-group`, then `x-order`). */
export function readSection(section: SettingsSection): FieldSpec[] {
	const schema: unknown = section.schema;
	const props = isObj(schema) && isObj(schema.properties) ? schema.properties : {};
	const specs: FieldSpec[] = [];
	for (const f of section.fields) {
		const p = props[f.name];
		const key = `${section.section}.${f.name}` as FieldKey;
		const prop: Obj = isObj(p) ? p : {};
		if (import.meta.env.DEV && !isObj(p)) devWarn(`${key} has no schema property`);
		const { kind, enum: values } = kindOf(prop);
		if (import.meta.env.DEV && kind === 'unknown')
			devWarn(`${key} has a schema shape the form can't read`);
		const danger = str(prop['x-danger']);
		const spec: FieldSpec = {
			key,
			section: section.section,
			name: f.name,
			labelKey: f.label_key,
			helpKey: f.help_key,
			kind,
			preview: prop['x-preview'] === true,
			group: str(prop['x-group']) ?? 'general',
			order: num(prop['x-order']) ?? Number.MAX_SAFE_INTEGER,
			advanced: prop['x-advanced'] === true,
			writeOnly: prop.writeOnly === true,
			readOnly: f.read_only || prop.readOnly === true,
			restartRequired: f.restart_required
		};
		if (values) spec.enum = values;
		const keywords = str(prop['x-keywords']);
		if (keywords) spec.keywordsKey = keywords;
		const min = num(prop.minimum);
		const max = num(prop.maximum);
		if (min !== undefined) spec.minimum = min;
		if (max !== undefined) spec.maximum = max;
		const format = str(prop.format);
		if (format) spec.format = format;
		const unit = str(prop['x-unit']);
		if (unit) spec.unit = unit;
		const widget = str(prop['x-widget']);
		if (widget) spec.widget = widget;
		if ('x-auto' in prop) spec.auto = prop['x-auto'];
		if (danger !== undefined) {
			if (DANGERS.includes(danger)) spec.danger = danger as Danger;
			// An unknown danger class must not pass as safe: treat it as the strictest.
			else {
				if (import.meta.env.DEV) devWarn(`${key} has an unknown x-danger "${danger}"`);
				spec.danger = 'security';
			}
		}
		const config = str(prop['x-config']);
		if (config) spec.config = config;
		const env = str(prop['x-env']) ?? f.env ?? undefined;
		if (env) spec.env = env;
		specs.push(spec);
	}
	return sortSpecs(specs);
}

/** Groups in order of their first field's `x-order`, fields by `x-order` within a group. */
function sortSpecs(specs: FieldSpec[]): FieldSpec[] {
	const first = new Map<string, number>();
	for (const s of specs) first.set(s.group, Math.min(first.get(s.group) ?? Infinity, s.order));
	return specs
		.map((s, i) => ({ s, i }))
		.sort(
			(a, b) =>
				first.get(a.s.group)! - first.get(b.s.group)! ||
				(a.s.group < b.s.group ? -1 : a.s.group > b.s.group ? 1 : 0) ||
				a.s.order - b.s.order ||
				a.i - b.i
		)
		.map(({ s }) => s);
}

export interface SettingsIndexView {
	sections: { section: string; labelKey: string; helpKey: string }[];
	spec(key: FieldKey): FieldSpec | undefined;
	/** The fields of these sections, in the order given, each in schema order. */
	fieldsOf(sections: string[]): FieldSpec[];
}

export function indexView(index: SettingsIndex): SettingsIndexView {
	const bySection = new Map<string, FieldSpec[]>();
	const byKey = new Map<string, FieldSpec>();
	for (const s of index.sections) {
		const specs = readSection(s);
		bySection.set(s.section, specs);
		for (const spec of specs) byKey.set(spec.key, spec);
	}
	return {
		sections: index.sections.map((s) => ({
			section: s.section,
			labelKey: s.label_key,
			helpKey: s.help_key
		})),
		spec: (key) => byKey.get(key),
		fieldsOf: (sections) => sections.flatMap((s) => bySection.get(s) ?? [])
	};
}

let cached: Promise<SettingsIndexView> | undefined;

/** `GET /settings`, once per tab. A failure isn't cached, so the next call retries. */
export function loadSettingsIndex(): Promise<SettingsIndexView> {
	cached ??= getIndex().then(indexView, (e: unknown) => {
		cached = undefined;
		throw e;
	});
	return cached;
}

/** Tests only: forget the cached index. */
export function clearIndexCache(): void {
	cached = undefined;
}

export interface FieldGroup {
	group: string;
	/** The group heading's server message key (`settings.group.<id>`). */
	labelKey: string;
	essentials: FieldSpec[];
	advanced: FieldSpec[];
}

/** Groups in the order their fields come, each split into essentials and Advanced. */
export function groupFields(specs: FieldSpec[]): FieldGroup[] {
	const groups = new Map<string, FieldGroup>();
	for (const s of specs) {
		let g = groups.get(s.group);
		if (!g) {
			g = { group: s.group, labelKey: `settings.group.${s.group}`, essentials: [], advanced: [] };
			groups.set(s.group, g);
		}
		(s.advanced ? g.advanced : g.essentials).push(s);
	}
	return [...groups.values()];
}

/** The field wrapper's element id and URL hash: `crawler.intensity`. */
export function fieldAnchor(key: FieldKey): string {
	return key;
}
