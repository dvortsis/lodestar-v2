// Settings → Logging's rules (§12.1, §10.10.12), mirroring the server's (`settings/logging.rs`,
// `logging/state.rs`): which events a staged verbosity turns on, what each data kind's switch is,
// and the `PATCH` a privacy change sends. The server enforces every deadline: the UI computes one
// only to send it, and shows what `GET /logs/catalog` says is on.
import type { components } from '$lib/api/schema';
import { ahead } from './state';

export { ahead, identifyingOn, optInOn, soonest } from './state';

export type LogEvent = components['schemas']['LogEvent'];
export type LogKind = components['schemas']['LogKind'];
export type LogLevel = components['schemas']['LogLevel'];
export type DataKind = components['schemas']['LogDataKind'];
export type Catalog = components['schemas']['LogCatalog'];

export const LEVELS: readonly LogLevel[] = ['error', 'warn', 'info', 'debug', 'trace'];
export const VERBOSITY = ['errors', 'warnings', 'normal', 'detailed', 'everything'] as const;
export type Verbosity = (typeof VERBOSITY)[number];

/** The two opt-ins and the settings field that holds each one's deadline. */
export const OPT_INS = {
	peer_activity: 'peer_activity_until',
	filter_changes: 'change_details_until'
} as const;
export type OptIn = keyof typeof OPT_INS;

export const HOUR = 3600;
export const DAY = 24 * HOUR;
/** A deadline: 0 off, -1 until turned off, else Unix seconds. */
export const UNTIL_OFF = -1;
export type Duration = 'hour' | 'day' | 'off';

const rank = (l: LogLevel) => LEVELS.indexOf(l);
export const verbosityLevel = (v: string): LogLevel =>
	LEVELS[Math.max(0, VERBOSITY.indexOf(v as Verbosity))] ?? 'info';

/** The level an event's component records at: its own, else the verbosity's. */
export function componentLevel(
	e: Pick<LogEvent, 'component'>,
	verbosity: string,
	components: Record<string, string>
): LogLevel {
	const own = components[e.component];
	return LEVELS.includes(own as LogLevel) ? (own as LogLevel) : verbosityLevel(verbosity);
}

/**
 * Whether the preset alone turns `e` on: at or above its component's level, and on by default
 * unless that level is debug or lower (`Event::on_under`). Opt-in events follow their opt-in.
 */
export function presetOn(
	e: LogEvent,
	verbosity: string,
	components: Record<string, string>
): boolean {
	if (e.required) return true;
	if (e.opt_in) return false;
	const max = componentLevel(e, verbosity, components);
	return rank(e.level) <= rank(max) && (e.default_on || rank(max) >= rank('debug'));
}

/** Whether `e` is on with these staged values (`Applied::event_on`, opt-ins aside). */
export function eventOn(
	e: LogEvent,
	verbosity: string,
	components: Record<string, string>,
	overrides: Record<string, boolean>
): boolean {
	if (e.required) return true;
	if (e.opt_in) return e.current;
	return overrides[e.id] ?? presetOn(e, verbosity, components);
}

/** The overrides after turning `e` on or off: one equal to the preset is dropped. */
export function toggleEvent(
	e: LogEvent,
	on: boolean,
	verbosity: string,
	components: Record<string, string>,
	overrides: Record<string, boolean>
): Record<string, boolean> {
	const next = { ...overrides };
	if (e.required || e.opt_in) return next;
	if (presetOn(e, verbosity, components) === on) delete next[e.id];
	else next[e.id] = on;
	return next;
}

/** The deadline a duration sets, on the server's clock. Opt-ins can't be "until turned off". */
export function deadline(d: Duration, now: number): number {
	return d === 'off' ? UNTIL_OFF : Math.floor(now) + (d === 'hour' ? HOUR : DAY);
}

/** The events that are on now and could carry `kind` (the "would appear in" line). */
export const carriers = (c: Catalog, kind: DataKind) =>
	c.events.filter((e) => e.current && e.kinds.includes(kind));

/** The saved values of the data fields a privacy change starts from. */
export interface DataValues {
	data: string;
	data_until: number;
	kinds: Record<string, number>;
}

/** Whether the preset alone has `kind` on (`DataPreset::has`, its deadline aside). */
export function presetHas(preset: string, kind: LogKind): boolean {
	if (kind.opt_in) return false;
	if (kind.default_on) return true;
	return preset === 'full' || (preset === 'hashes' && kind.kind === 'hash');
}

/**
 * The patch that turns one kind on (with its deadline) or off. A non-identifying kind is on or
 * off for good (`-1`/`0`); a switch equal to what the preset gives is dropped from `kinds`.
 */
export function kindPatch(
	v: DataValues,
	kind: LogKind,
	on: boolean,
	until: number,
	now: number
): Pick<DataValues, 'kinds'> {
	const kinds = { ...v.kinds };
	const fromPreset = presetHas(v.data, kind) && (!kind.identifying || ahead(v.data_until, now));
	if (on === fromPreset && !(on && kind.identifying)) delete kinds[kind.kind];
	else kinds[kind.kind] = on ? (kind.identifying ? until : UNTIL_OFF) : 0;
	return { kinds };
}

/** The patch for a data preset: its deadline, and no per-kind switches. */
export function presetPatch(preset: string, until: number): DataValues {
	return { data: preset, data_until: preset === 'private' ? 0 : until, kinds: {} };
}

/**
 * The patch that turns every identifying kind off ("Turn off now"): back to Private, keeping only
 * the switches of kinds that don't identify anyone (client addresses, sizes).
 */
export function offPatch(v: DataValues, c: Catalog): DataValues {
	const keep = new Set(c.kinds.filter((k) => !k.identifying && !k.opt_in).map((k) => k.kind));
	const kinds = Object.fromEntries(
		Object.entries(v.kinds).filter(([k]) => keep.has(k as DataKind))
	);
	return { data: 'private', data_until: 0, kinds };
}

/** Whether a preset choice needs the privacy dialog: any but Private turns on identifying kinds. */
export const presetIdentifies = (preset: string) => preset !== 'private';
