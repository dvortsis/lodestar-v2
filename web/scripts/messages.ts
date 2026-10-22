// Merges the per-feature message files into the two files Paraglide reads
// (messages/{en,zh}.json). One file per feature means translations never conflict.
//
//   src/lib/features/<feature>/messages/{en,zh}.json   keys start with "<feature>_"
//   src/lib/i18n/messages/{en,zh}.json                 shared keys: glossary terms (bare) and common_*
//
// Errors (exit 1): unreadable or non-string messages, bad key names, a key defined twice,
// a zh key with no en key, a bad placeholder, zh placeholders that differ from en.
// Warning: en keys missing from zh (zh is filled later), listed by key.
import { existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

export const LOCALES = ['en', 'zh'] as const;
export type Locale = (typeof LOCALES)[number];

// Shared messages are the glossary terms (bare keys, as docs/ui-glossary.md specifies) and common_*.
export const SHARED_PREFIXES = [''];

const KEY = /^[a-z][a-z0-9]*(_[a-z0-9]+)*$/;
const PLACEHOLDER_NAME = /^[A-Za-z_][A-Za-z0-9_]*$/;

/** zh is only auto-selected from the browser when it covers at least this share of en. */
export const AUTO_LOCALE_COVERAGE = 0.95;

/** Top-level keys of a JSON object in file order, repeats included (JSON.parse drops them). */
export function topLevelKeys(text: string): string[] {
	const keys: string[] = [];
	let depth = 0;
	for (let i = 0; i < text.length; i++) {
		const c = text[i];
		if (c === '{' || c === '[') depth++;
		else if (c === '}' || c === ']') depth--;
		else if (c === '"') {
			let j = i + 1;
			while (j < text.length && text[j] !== '"') j += text[j] === '\\' ? 2 : 1;
			if (depth === 1 && /^\s*:/.test(text.slice(j + 1, j + 64))) {
				keys.push(JSON.parse(text.slice(i, j + 1)) as string);
			}
			i = j;
		}
	}
	return keys;
}

export type Source = { name: string; prefixes: string[]; dir: string };
export type MergeResult = {
	messages: Record<Locale, Record<string, string>>;
	/** Share of en keys that have a message in each locale (en is 1). */
	coverage: Record<Locale, number>;
	errors: string[];
	warnings: string[];
};

/** Placeholder names in a Paraglide message string, or the reason it is invalid. */
export function placeholders(text: string): { names: string[] } | { error: string } {
	const names: string[] = [];
	for (let i = 0; i < text.length; i++) {
		const c = text[i];
		if (c === '}') return { error: `unmatched "}" at ${i}` };
		if (c !== '{') continue;
		const end = text.indexOf('}', i);
		if (end < 0) return { error: `unclosed "{" at ${i}` };
		const inner = text.slice(i + 1, end);
		if (inner.includes('{')) return { error: `nested "{" at ${i}` };
		if (inner.includes(',')) {
			return { error: `ICU syntax "{${inner}}" is not supported; use a plain {name} placeholder` };
		}
		if (!PLACEHOLDER_NAME.test(inner)) return { error: `invalid placeholder "{${inner}}"` };
		names.push(inner);
		i = end;
	}
	return { names };
}

/** Feature directories and the shared directory under `root` (the web/ folder). */
export function discover(root: string): Source[] {
	const sources: Source[] = [
		{ name: 'i18n', prefixes: SHARED_PREFIXES, dir: join(root, 'src/lib/i18n/messages') }
	];
	const features = join(root, 'src/lib/features');
	if (existsSync(features)) {
		for (const entry of readdirSync(features, { withFileTypes: true })) {
			if (!entry.isDirectory()) continue;
			sources.push({
				name: entry.name,
				prefixes: [`${entry.name.replaceAll('-', '_')}_`],
				dir: join(features, entry.name, 'messages')
			});
		}
	}
	return sources.sort((a, b) => a.name.localeCompare(b.name));
}

export function merge(sources: Source[]): MergeResult {
	const errors: string[] = [];
	const warnings: string[] = [];
	const messages: Record<Locale, Record<string, string>> = { en: {}, zh: {} };
	const owner: Record<Locale, Record<string, string>> = { en: {}, zh: {} };

	for (const source of sources) {
		for (const locale of LOCALES) {
			const file = join(source.dir, `${locale}.json`);
			if (!existsSync(file)) {
				if (existsSync(source.dir) && locale === 'en') errors.push(`${file}: missing`);
				continue;
			}
			let data: unknown;
			try {
				const text = readFileSync(file, 'utf8');
				data = JSON.parse(text);
				const seen = new Set<string>();
				for (const key of topLevelKeys(text)) {
					if (seen.has(key)) errors.push(`${file}: "${key}": duplicate key in the same file`);
					seen.add(key);
				}
			} catch (e) {
				errors.push(`${file}: ${(e as Error).message}`);
				continue;
			}
			if (typeof data !== 'object' || data === null || Array.isArray(data)) {
				errors.push(`${file}: must be a JSON object`);
				continue;
			}
			for (const [key, value] of Object.entries(data)) {
				if (key === '$schema') continue;
				const where = `${file}: "${key}"`;
				if (typeof value !== 'string') {
					errors.push(`${where}: message must be a string`);
					continue;
				}
				if (!KEY.test(key)) {
					errors.push(`${where}: key must be snake_case (a-z, 0-9, _)`);
					continue;
				}
				if (!source.prefixes.some((p) => key.startsWith(p))) {
					errors.push(
						`${where}: key must start with ${source.prefixes.map((p) => `"${p}"`).join(' or ')}`
					);
					continue;
				}
				if (key in messages[locale]) {
					errors.push(`${where}: duplicate key, already defined by ${owner[locale][key]}`);
					continue;
				}
				const parsed = placeholders(value);
				if ('error' in parsed) {
					errors.push(`${where}: ${parsed.error}`);
					continue;
				}
				messages[locale][key] = value;
				owner[locale][key] = source.name;
			}
		}
	}

	for (const key of Object.keys(messages.zh)) {
		const en = messages.en[key];
		if (en === undefined) {
			errors.push(`zh key "${key}" (${owner.zh[key]}) has no en message`);
			continue;
		}
		const a = [...new Set((placeholders(en) as { names: string[] }).names)].sort();
		const b = [...new Set((placeholders(messages.zh[key]!) as { names: string[] }).names)].sort();
		if (a.join() !== b.join()) {
			errors.push(`"${key}": zh placeholders {${b.join(', ')}} differ from en {${a.join(', ')}}`);
		}
	}

	const untranslated = Object.keys(messages.en).filter((k) => !(k in messages.zh));
	if (untranslated.length) {
		warnings.push(`${untranslated.length} en message(s) not in zh yet: ${untranslated.join(', ')}`);
	}
	const total = Object.keys(messages.en).length;
	const share = (n: number) => (total === 0 ? 1 : n / total);
	const coverage = {
		en: 1,
		zh: share(Object.keys(messages.zh).filter((k) => k in messages.en).length)
	};
	return { messages, coverage, errors, warnings };
}

/** Merges and writes messages/{en,zh}.json under `root`; returns the result. */
export function mergeMessages(root: string): MergeResult {
	const result = merge(discover(root));
	if (result.errors.length) return result;
	const out = join(root, 'messages');
	mkdirSync(out, { recursive: true });
	for (const locale of LOCALES) {
		const sorted = Object.fromEntries(
			Object.entries(result.messages[locale]).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
		);
		const text = JSON.stringify({ $schema: SCHEMA, ...sorted }, null, '\t') + '\n';
		const file = join(out, `${locale}.json`);
		if (!existsSync(file) || readFileSync(file, 'utf8') !== text) writeFileSync(file, text);
	}
	// Read by src/lib/i18n/locale.svelte.ts: which locales may be auto-selected from the browser.
	const covered = LOCALES.map((l) => `${l}: ${result.coverage[l].toFixed(4)}`).join(', ');
	const generated =
		'// Generated by scripts/messages.ts; do not edit.\n' +
		`export const AUTO_LOCALE_COVERAGE = ${AUTO_LOCALE_COVERAGE};\n` +
		`export const coverage: Record<string, number> = { ${covered} };\n`;
	const coverageFile = join(root, 'src/lib/i18n/coverage.generated.ts');
	mkdirSync(dirname(coverageFile), { recursive: true });
	if (!existsSync(coverageFile) || readFileSync(coverageFile, 'utf8') !== generated) {
		writeFileSync(coverageFile, generated);
	}
	return result;
}

const SCHEMA = 'https://inlang.com/schema/inlang-message-format';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
	const { errors, warnings, messages } = mergeMessages(root);
	for (const w of warnings) console.warn(`messages: warning: ${w}`);
	if (errors.length) {
		console.error(errors.map((e) => `messages: ${e}`).join('\n'));
		console.error(`\n${errors.length} message error(s).`);
		process.exit(1);
	}
	console.log(
		`messages: ok (${Object.keys(messages.en).length} en, ${Object.keys(messages.zh).length} zh)`
	);
}
