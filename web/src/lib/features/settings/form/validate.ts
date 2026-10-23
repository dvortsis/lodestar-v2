// Client validation from the schema (§10.10.5): type, integer, bounds, enum, the address
// formats and time windows. The server has the final say; `log-filter` and `duration` are left
// to it and to their widgets. Messages say what's wrong and what to do (§10.14).
import { formatCount } from '$lib/format';
import { currentLocale } from '$lib/i18n/locale.svelte';
import { m } from '$lib/paraglide/messages.js';
import { unitText } from './units';
import type { FieldSpec } from './types';
import { JsonDraft, RESET, type Staged } from './values';

export { JsonDraft };

const IPV4 = /^(25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)(\.(25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)){3}$/;
const HOST_LABEL = /^(?!-)[A-Za-z0-9-]{1,63}(?<!-)$/;

export function isIPv4(s: string): boolean {
	return IPV4.test(s);
}

/** An IPv6 address without brackets: hex groups, one `::`, an optional IPv4 tail, no zone. */
export function isIPv6(s: string): boolean {
	if (!/^[0-9A-Fa-f:.]+$/.test(s) || s.length > 45) return false;
	const halves = s.split('::');
	if (halves.length > 2) return false;
	const groups = (part: string) => (part === '' ? [] : part.split(':'));
	const parts = halves.flatMap(groups);
	let count = 0;
	for (const [i, p] of parts.entries()) {
		if (i === parts.length - 1 && p.includes('.')) {
			if (!isIPv4(p)) return false;
			count += 2;
		} else if (/^[0-9A-Fa-f]{1,4}$/.test(p)) count++;
		else return false;
	}
	return halves.length === 2 ? count < 8 : count === 8;
}

function isPort(s: string): boolean {
	if (!/^\d{1,5}$/.test(s)) return false;
	const n = Number(s);
	return n >= 1 && n <= 65535;
}

/** `IPv4:port` or `[IPv6]:port`, port 1–65535 (what Rust's `SocketAddr` parses). */
export function isSocketAddr(s: string): boolean {
	const v6 = /^\[([^\]]+)\]:(\d+)$/.exec(s);
	if (v6) return isIPv6(v6[1]!) && isPort(v6[2]!);
	const i = s.lastIndexOf(':');
	return i > 0 && isIPv4(s.slice(0, i)) && isPort(s.slice(i + 1));
}

export function isHost(s: string): boolean {
	if (isIPv4(s)) return true;
	if (s.startsWith('[') && s.endsWith(']')) return isIPv6(s.slice(1, -1));
	if (isIPv6(s)) return true;
	const name = s.endsWith('.') ? s.slice(0, -1) : s;
	return name.length > 0 && name.length <= 253 && name.split('.').every((l) => HOST_LABEL.test(l));
}

const TIME_RANGE = /^([01]\d|2[0-3]):([0-5]\d)-([01]\d|2[0-3]):([0-5]\d)$/;

/**
 * A time window, `HH:MM-HH:MM` in 24-hour time (what B-update's server parses): `ok`; `same`
 * when it starts and ends at the same minute (no time, or all of it: the server refuses it);
 * else `bad`. An end before the start runs past midnight.
 */
export function timeRange(s: string): 'ok' | 'same' | 'bad' {
	const t = TIME_RANGE.exec(s);
	if (!t) return 'bad';
	return t[1] === t[3] && t[2] === t[4] ? 'same' : 'ok';
}

export function isHttpUrl(s: string): boolean {
	try {
		const u = new URL(s);
		return (u.protocol === 'http:' || u.protocol === 'https:') && u.hostname !== '';
	} catch {
		return false;
	}
}

function boundsMessage(spec: FieldSpec): string {
	const locale = currentLocale();
	const unit = unitText(spec.unit) ?? '';
	const min = spec.minimum === undefined ? undefined : formatCount(spec.minimum, locale);
	const max = spec.maximum === undefined ? undefined : formatCount(spec.maximum, locale);
	if (min !== undefined && max !== undefined) {
		return unit
			? m.settings_form_error_between_unit({ min, max, unit })
			: m.settings_form_error_between({ min, max });
	}
	if (min !== undefined) {
		return unit
			? m.settings_form_error_at_least_unit({ min, unit })
			: m.settings_form_error_at_least({ min });
	}
	return unit
		? m.settings_form_error_at_most_unit({ max: max!, unit })
		: m.settings_form_error_at_most({ max: max! });
}

/** The error for a staged value, or undefined when the client finds nothing wrong. */
export function validate(spec: FieldSpec, value: Staged): string | undefined {
	if (value === RESET || spec.writeOnly) return undefined;
	if (value instanceof JsonDraft) return m.settings_form_json_invalid();
	switch (spec.kind) {
		case 'boolean':
			return typeof value === 'boolean' ? undefined : m.settings_form_error_type();
		case 'enum':
			return typeof value === 'string' && spec.enum?.includes(value)
				? undefined
				: m.settings_form_error_enum();
		case 'integer':
		case 'number': {
			if (typeof value !== 'number' || !Number.isFinite(value))
				return m.settings_form_error_number();
			if (spec.kind === 'integer' && !Number.isInteger(value))
				return m.settings_form_error_integer();
			const low = spec.minimum !== undefined && value < spec.minimum;
			const high = spec.maximum !== undefined && value > spec.maximum;
			return low || high ? boundsMessage(spec) : undefined;
		}
		case 'string': {
			if (typeof value !== 'string') return m.settings_form_error_type();
			// An `x-auto` empty string (follow the browser) is always valid.
			if (value === '' && spec.auto === '') return undefined;
			if (spec.format === 'socket-addr' && !isSocketAddr(value)) {
				return m.settings_form_error_socket_addr();
			}
			if (spec.format === 'url' && !isHttpUrl(value)) return m.settings_form_error_url();
			if (spec.format === 'host' && !isHost(value)) return m.settings_form_error_host();
			if (spec.format === 'time-range') {
				const t = timeRange(value);
				if (t === 'bad') return m.settings_form_error_time_range();
				if (t === 'same') return m.settings_form_error_time_range_same();
			}
			return undefined;
		}
		case 'string-list':
			return Array.isArray(value) && value.every((v) => typeof v === 'string')
				? undefined
				: m.settings_form_error_type();
		case 'map':
			return typeof value === 'object' && value !== null && !Array.isArray(value)
				? undefined
				: m.settings_form_error_type();
		default:
			return undefined;
	}
}
