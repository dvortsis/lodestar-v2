// The settings form, generated from a source's `config_schema` (SRC-15c, §18.8). The server has
// already left secrets out of it. It reads the JSON Schema subset the plugins use: string (with
// enum, minLength, maxLength), integer and number (minimum, maximum), boolean, and arrays of strings
// or of flat objects of those (minItems, maxItems, uniqueItems). Anything else becomes an
// `unsupported` field: the form says "edit through the API" and sends nothing for it. The checks
// here only save a round trip: the server judges every save, and its answer is always shown.
import { m } from '$lib/paraglide/messages.js';

export type Scalar = 'string' | 'integer' | 'number' | 'boolean' | 'enum';
interface Base {
	name: string;
	label: string;
	help: string;
	required: boolean;
}
export interface ScalarField extends Base {
	kind: Scalar;
	min?: number;
	max?: number;
	minLength?: number;
	maxLength?: number;
	options: string[];
	default?: unknown;
}
export interface ListField extends Base {
	kind: 'strings' | 'objects';
	/** The element: a string field, or the object's flat fields. */
	item: ScalarField[];
	minItems?: number;
	maxItems?: number;
	unique: boolean;
	default?: unknown[];
}
export interface UnsupportedField extends Base {
	kind: 'unsupported';
}
export type Field = ScalarField | ListField | UnsupportedField;

export const isList = (f: Field): f is ListField => f.kind === 'strings' || f.kind === 'objects';

type Json = Record<string, unknown>;
const isObj = (v: unknown): v is Json => typeof v === 'object' && v !== null && !Array.isArray(v);
const num = (v: unknown) => (typeof v === 'number' && Number.isFinite(v) ? v : undefined);

function scalar(name: string, s: Json, required: boolean, title?: string): ScalarField | null {
	const base = {
		name,
		label: typeof s.title === 'string' ? s.title : (title ?? name),
		help: typeof s.description === 'string' ? s.description : '',
		required,
		min: num(s.minimum),
		max: num(s.maximum),
		minLength: num(s.minLength),
		maxLength: num(s.maxLength),
		options: [] as string[],
		default: s.default
	};
	if (s.type === 'string') {
		if (Array.isArray(s.enum)) {
			if (!s.enum.every((o) => typeof o === 'string')) return null;
			return { ...base, kind: 'enum', options: s.enum as string[] };
		}
		return s.enum === undefined ? { ...base, kind: 'string' } : null;
	}
	if ((s.type === 'integer' || s.type === 'number') && s.enum === undefined)
		return { ...base, kind: s.type };
	if (s.type === 'boolean') return { ...base, kind: 'boolean' };
	return null;
}

/** The form's fields in schema order; null when the schema isn't an object with properties. */
export function parseSchema(schema: unknown): Field[] | null {
	if (!isObj(schema) || schema.type !== 'object' || !isObj(schema.properties)) return null;
	const req = new Set(Array.isArray(schema.required) ? (schema.required as unknown[]) : []);
	return Object.entries(schema.properties).map(([name, raw]): Field => {
		const required = req.has(name);
		const s = isObj(raw) ? raw : {};
		const unsupported: UnsupportedField = {
			name,
			kind: 'unsupported',
			label: typeof s.title === 'string' ? s.title : name,
			help: '',
			required
		};
		if (s.type === 'array' && isObj(s.items)) {
			const it = s.items;
			let item: ScalarField[] | null = null;
			if (it.type === 'string') {
				const f = scalar('', it, true);
				item = f && f.kind === 'string' ? [f] : null;
			} else if (it.type === 'object' && isObj(it.properties)) {
				const inner = new Set(Array.isArray(it.required) ? (it.required as unknown[]) : []);
				const fs = Object.entries(it.properties).map(([k, v]) =>
					isObj(v) ? scalar(k, v, inner.has(k)) : null
				);
				item = fs.every((f) => f !== null) && fs.length > 0 ? (fs as ScalarField[]) : null;
			}
			if (!item) return unsupported;
			return {
				name,
				kind: it.type === 'string' ? 'strings' : 'objects',
				label: typeof s.title === 'string' ? s.title : name,
				help: typeof s.description === 'string' ? s.description : '',
				required,
				item,
				minItems: num(s.minItems),
				maxItems: num(s.maxItems),
				unique: s.uniqueItems === true,
				default: Array.isArray(s.default) ? s.default : undefined
			};
		}
		return scalar(name, s, required) ?? unsupported;
	});
}

/** What the form edits: text and numbers as typed, booleans, and lists of rows of the same. */
export type Row = Record<string, string | boolean>;
export type Values = Record<string, string | boolean | (string | Row)[]>;

const initial = (f: ScalarField): string | boolean =>
	f.kind === 'boolean'
		? f.default === true
		: f.default === undefined || f.default === null
			? ''
			: String(f.default);

/** A row nobody filled in: every cell still holds its default (or is empty). */
export const isBlankRow = (f: ListField, r: Row): boolean =>
	f.item.every((i) =>
		i.kind === 'boolean' ? r[i.name] === false : String(r[i.name] ?? '').trim() === ''
	);

export const blankRow = (f: ListField): Row =>
	Object.fromEntries(f.item.map((i) => [i.name, initial(i)]));

const listDefault = (f: ListField): (string | Row)[] =>
	(f.default ?? []).flatMap((d): (string | Row)[] => {
		if (f.kind === 'strings') return typeof d === 'string' ? [d] : [];
		if (!isObj(d)) return [];
		const row = blankRow(f);
		for (const i of f.item)
			if (d[i.name] !== undefined)
				row[i.name] = i.kind === 'boolean' ? d[i.name] === true : String(d[i.name]);
		return [row];
	});

/** The form's first values: each setting's default (the API doesn't return saved values). */
export function initialValues(fields: Field[]): Values {
	const out: Values = {};
	for (const f of fields) {
		if (f.kind === 'unsupported') continue;
		out[f.name] = isList(f) ? listDefault(f) : initial(f);
	}
	return out;
}

export type Errors = Record<string, string>;
/** The error key of a list element's cell: `caches.2` or `rows.2.url`. */
export const cellKey = (list: string, i: number, sub = '') =>
	sub ? `${list}.${i}.${sub}` : `${list}.${i}`;

function checkScalar(f: ScalarField, v: string | boolean): string {
	if (f.kind === 'boolean') return '';
	const t = String(v).trim();
	if (t === '') return f.required ? m.sources_cfg_required() : '';
	if (f.kind === 'integer' || f.kind === 'number') {
		const n = /^-?\d+(\.\d+)?$/.test(t) ? Number(t) : NaN;
		if (!Number.isFinite(n) || (f.kind === 'integer' && !Number.isInteger(n)))
			return f.kind === 'integer' ? m.sources_cfg_integer() : m.sources_cfg_number();
		if (f.min !== undefined && n < f.min) return m.sources_cfg_min({ min: f.min });
		if (f.max !== undefined && n > f.max) return m.sources_cfg_max({ max: f.max });
		return '';
	}
	if (f.kind === 'enum') return f.options.includes(t) ? '' : m.sources_cfg_choose();
	if (f.minLength !== undefined && t.length < f.minLength)
		return m.sources_cfg_short({ min: f.minLength });
	if (f.maxLength !== undefined && t.length > f.maxLength)
		return m.sources_cfg_long({ max: f.maxLength });
	return '';
}

/** Checks the way the server does (same bounds); keyed by field name or `cellKey`. Empty = valid. */
export function validate(fields: Field[], values: Values): Errors {
	const out: Errors = {};
	for (const f of fields) {
		if (f.kind === 'unsupported') continue;
		const v = values[f.name];
		if (!isList(f)) {
			const e = checkScalar(f, v as string | boolean);
			if (e) out[f.name] = e;
			continue;
		}
		const rows = (v as (string | Row)[]).filter((r) =>
			typeof r === 'string'
				? r.trim() !== ''
				: Object.values(r).some((x) => x !== '' && x !== false)
		);
		const n = rows.length;
		if (f.minItems !== undefined && n < f.minItems)
			out[f.name] = m.sources_cfg_min_items({ min: f.minItems });
		else if (f.maxItems !== undefined && n > f.maxItems)
			out[f.name] = m.sources_cfg_max_items({ max: f.maxItems });
		const seen = new Set<string>();
		(v as (string | Row)[]).forEach((r, i) => {
			if (typeof r === 'string') {
				if (r.trim() === '') return;
				const e = checkScalar({ ...f.item[0]!, required: true }, r);
				if (e) out[cellKey(f.name, i)] = e;
				else if (f.unique) {
					if (seen.has(r.trim())) out[cellKey(f.name, i)] = m.sources_cfg_duplicate();
					seen.add(r.trim());
				}
				return;
			}
			if (!rows.includes(r)) return;
			for (const sub of f.item) {
				const e = checkScalar(sub, r[sub.name] ?? '');
				if (e) out[cellKey(f.name, i, sub.name)] = e;
			}
			if (f.unique) {
				const key = JSON.stringify(f.item.map((s) => r[s.name]));
				if (seen.has(key)) out[cellKey(f.name, i)] = m.sources_cfg_duplicate();
				seen.add(key);
			}
		});
	}
	return out;
}

function toScalar(f: ScalarField, v: string | boolean): unknown {
	if (f.kind === 'boolean') return v === true;
	const t = String(v).trim();
	if (t === '') return undefined;
	return f.kind === 'integer' || f.kind === 'number' ? Number(t) : t;
}

/** The `config` body: typed values, blank optional settings and blank list rows left out. */
export function toConfig(fields: Field[], values: Values): Json {
	const out: Json = {};
	for (const f of fields) {
		if (f.kind === 'unsupported') continue;
		const v = values[f.name];
		if (!isList(f)) {
			const x = toScalar(f, v as string | boolean);
			if (x !== undefined) out[f.name] = x;
		} else if (f.kind === 'strings') {
			out[f.name] = (v as string[]).map((s) => s.trim()).filter((s) => s !== '');
		} else if (f.kind === 'objects') {
			out[f.name] = (v as Row[])
				.filter((r) => !isBlankRow(f, r))
				.map((r) => {
					const o: Json = {};
					for (const s of f.item) {
						const x = toScalar(s, r[s.name] ?? '');
						if (x !== undefined) o[s.name] = x;
					}
					return o;
				});
		}
	}
	return out;
}
