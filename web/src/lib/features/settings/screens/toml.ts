// View as config.toml (§10.10.2): a screen's values that aren't at their default, as `config.toml`
// lines (`dht.intensity = "low"  # LODESTAR_INTENSITY`, dotted keys at the top level are valid TOML)
// and as `LODESTAR_*` lines for a compose file or an Unraid template. Strings use TOML basic-string
// escaping; lists and maps are inline. Secrets never appear: the caller leaves out `writeOnly`.

const BARE_KEY = /^[A-Za-z0-9_-]+$/;

/** A TOML basic string: `"`, `\` and control characters escaped, everything else as written. */
export function tomlString(s: string): string {
	let out = '"';
	for (let i = 0; i < s.length; i++) {
		const c = s.charCodeAt(i);
		const ch = s[i]!;
		if (ch === '"') out += '\\"';
		else if (ch === '\\') out += '\\\\';
		else if (ch === '\n') out += '\\n';
		else if (ch === '\r') out += '\\r';
		else if (ch === '\t') out += '\\t';
		else if (ch === '\b') out += '\\b';
		else if (ch === '\f') out += '\\f';
		else if (c < 0x20 || c === 0x7f) out += `\\u${c.toString(16).padStart(4, '0').toUpperCase()}`;
		else if (c >= 0xd800 && c <= 0xdbff) {
			// A surrogate pair is one character; a lone surrogate isn't valid TOML.
			const next = s.charCodeAt(i + 1);
			if (next >= 0xdc00 && next <= 0xdfff) {
				out += ch + s[i + 1];
				i++;
			} else out += '\\uFFFD';
		} else if (c >= 0xdc00 && c <= 0xdfff) out += '\\uFFFD';
		else out += ch;
	}
	return `${out}"`;
}

/** A key part: bare when it can be, else a quoted string. */
export function tomlKey(k: string): string {
	return BARE_KEY.test(k) ? k : tomlString(k);
}

/** A dotted key (`dht.intensity`), each part quoted only when it must be. */
export function tomlDottedKey(key: string): string {
	return key.split('.').map(tomlKey).join('.');
}

/** A value as TOML, or undefined when it has no TOML form (`null`, `undefined`, a function). */
export function tomlValue(v: unknown): string | undefined {
	if (typeof v === 'string') return tomlString(v);
	if (typeof v === 'boolean') return v ? 'true' : 'false';
	if (typeof v === 'number') {
		if (Number.isNaN(v)) return 'nan';
		if (!Number.isFinite(v)) return v > 0 ? 'inf' : '-inf';
		return Object.is(v, -0) ? '-0' : String(v);
	}
	if (typeof v === 'bigint') return v.toString();
	if (Array.isArray(v)) {
		const items = v.map(tomlValue);
		// TOML has no null: an array with one can't be written faithfully, so none of it is.
		return items.every((i) => i !== undefined) ? `[${items.join(', ')}]` : undefined;
	}
	if (typeof v === 'object' && v !== null) {
		const entries = Object.entries(v).map(([k, x]) => [k, tomlValue(x)] as const);
		if (entries.some(([, x]) => x === undefined)) return undefined;
		return entries.length
			? `{ ${entries.map(([k, x]) => `${tomlKey(k)} = ${x}`).join(', ')} }`
			: '{}';
	}
	return undefined;
}

/**
 * A value as an environment variable's text. Scalars only (the server reads a list or a map from
 * `config.toml`, not from one variable). A value with a space, quote, `#` or `$` is double-quoted
 * so a compose file or an `.env` file reads it back whole.
 */
export function envValue(v: unknown): string | undefined {
	let text: string;
	if (typeof v === 'string') text = v;
	else if (typeof v === 'boolean') text = v ? 'true' : 'false';
	else if (typeof v === 'number' && Number.isFinite(v)) text = String(v);
	else return undefined;
	if (/[\s"'#$\\]|^$/.test(text)) {
		return `"${text
			.replace(/[\\"$]/g, '\\$&')
			.replace(/\n/g, '\\n')
			.replace(/\r/g, '\\r')}"`;
	}
	return text;
}

export interface ConfigEntry {
	/** `x-config`, e.g. `dht.intensity`; absent for a setting only the app saves. */
	config?: string;
	/** `LODESTAR_*`, e.g. `LODESTAR_INTENSITY`. */
	env?: string;
	value: unknown;
}

/** The `config.toml` lines, in the order given. */
export function configToml(entries: readonly ConfigEntry[]): string {
	const lines: string[] = [];
	for (const e of entries) {
		const value = e.config ? tomlValue(e.value) : undefined;
		if (!e.config || value === undefined) continue;
		lines.push(`${tomlDottedKey(e.config)} = ${value}${e.env ? `  # ${e.env}` : ''}`);
	}
	return lines.join('\n');
}

/** The `LODESTAR_X=value` lines for a compose file or an Unraid template. */
export function envLines(entries: readonly ConfigEntry[]): string {
	const lines: string[] = [];
	for (const e of entries) {
		const value = e.env ? envValue(e.value) : undefined;
		if (e.env && value !== undefined) lines.push(`${e.env}=${value}`);
	}
	return lines.join('\n');
}
