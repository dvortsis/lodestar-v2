// eslint-disable-next-line @typescript-eslint/triple-slash-reference -- the ambient `virtual:licenses` declaration, for every program that loads vite.config.ts
/// <reference path="./licenses-virtual.d.ts" />
// Licences and credits for About (architecture §10.10.13, owner's decision §2.12).
//
// A Vite plugin. In the client build it takes the npm packages bundled into the UI from the
// module graph (JS and CSS, lazy chunks included), reads their licences from package-lock.json
// and their texts from each package's folder, merges licenses/rust.json (written by
// .github/scripts/licenses.py) and licenses/overrides.json, checks every licence against
// licenses/allow.json, and emits one content-hashed `licenses-<hash>.json`. Its URL is exported
// from `virtual:licenses` for About to fetch when its section opens; nothing loads it on first
// paint. `vite dev` serves the same document from the modules the browser has loaded so far.
//
// Node built-ins only (no new dependency). The SPDX evaluator mirrors licenses.py's.
import { createHash } from 'node:crypto';
import { existsSync, lstatSync, readdirSync, readFileSync, realpathSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join, posix } from 'node:path';
import type { Plugin, ResolvedConfig } from 'vite';

export const VIRTUAL_ID = 'virtual:licenses';
const RESOLVED_ID = '\0' + VIRTUAL_ID;
/** Stands in for the asset URL until the asset exists (replaced in renderChunk). */
const PLACEHOLDER = '__LODESTAR_LICENSES_URL__';
/** Where `vite dev` serves the document, under the dev base. */
export const DEV_PATH = '__lodestar/licenses.json';

// --- SPDX ---------------------------------------------------------------------------------------

/** Deprecated SPDX identifiers and their current form. */
export const DEPRECATED: Record<string, string> = {
	'GPL-1.0': 'GPL-1.0-only',
	'GPL-1.0+': 'GPL-1.0-or-later',
	'GPL-2.0': 'GPL-2.0-only',
	'GPL-2.0+': 'GPL-2.0-or-later',
	'GPL-3.0': 'GPL-3.0-only',
	'GPL-3.0+': 'GPL-3.0-or-later',
	'AGPL-1.0': 'AGPL-1.0-only',
	'AGPL-3.0': 'AGPL-3.0-only',
	'AGPL-3.0+': 'AGPL-3.0-or-later',
	'LGPL-2.0': 'LGPL-2.0-only',
	'LGPL-2.0+': 'LGPL-2.0-or-later',
	'LGPL-2.1': 'LGPL-2.1-only',
	'LGPL-2.1+': 'LGPL-2.1-or-later',
	'LGPL-3.0': 'LGPL-3.0-only',
	'LGPL-3.0+': 'LGPL-3.0-or-later',
	'GFDL-1.3': 'GFDL-1.3-only'
};

export type Spdx =
	| { op: 'or'; items: Spdx[] }
	| { op: 'and'; items: Spdx[] }
	| { op: 'with'; id: string; exception: string }
	| { op: 'id'; id: string };

const OPERATORS = new Set(['AND', 'OR', 'WITH']);

/** Parse an SPDX expression. Precedence, highest first: WITH, AND, OR. Operators match without
 * regard to case, and the old `MIT/Apache-2.0` form reads as `MIT OR Apache-2.0`. */
export function parseSpdx(expr: string): Spdx {
	const toks =
		expr
			.trim()
			.replace(/\s*\/\s*/g, ' OR ')
			.match(/\(|\)|[^\s()]+/g) ?? [];
	if (!toks.length) throw new Error('empty licence expression');
	let pos = 0;
	const peek = () => toks[pos];
	const isOp = (word: string) => peek()?.toUpperCase() === word;
	const take = () => {
		const t = toks[pos++];
		if (t === undefined) throw new Error(`unexpected end of '${expr}'`);
		return t;
	};
	const primary = (): Spdx => {
		const t = take();
		if (t === '(') {
			const node = disjunction();
			if (peek() !== ')') throw new Error(`unbalanced parentheses in '${expr}'`);
			take();
			return node;
		}
		if (t === ')' || OPERATORS.has(t.toUpperCase()))
			throw new Error(`unexpected '${t}' in '${expr}'`);
		if (isOp('WITH')) {
			take();
			const exception = peek();
			if (
				exception === undefined ||
				exception === '(' ||
				exception === ')' ||
				OPERATORS.has(exception.toUpperCase())
			)
				throw new Error(`WITH needs an exception in '${expr}'`);
			take();
			return { op: 'with', id: t, exception };
		}
		return { op: 'id', id: t };
	};
	const conjunction = (): Spdx => {
		const items = [primary()];
		while (isOp('AND')) {
			take();
			items.push(primary());
		}
		return items.length === 1 ? items[0]! : { op: 'and', items };
	};
	const disjunction = (): Spdx => {
		const items = [conjunction()];
		while (isOp('OR')) {
			take();
			items.push(conjunction());
		}
		return items.length === 1 ? items[0]! : { op: 'or', items };
	};
	const tree = disjunction();
	if (pos !== toks.length) throw new Error(`unexpected '${toks[pos]}' in '${expr}'`);
	return tree;
}

export interface AllowList {
	allowed: string[];
	refused: string[];
}

export interface Verdict {
	ok: boolean;
	reason: string;
}

/** `OR` passes if any branch passes, `AND` needs every branch, `X WITH exception` passes when X
 * does. Identifiers match without regard to case; deprecated ones are read as their current form. */
export function evaluate(expr: string, allow: AllowList): Verdict {
	let tree: Spdx;
	try {
		tree = parseSpdx(expr);
	} catch (e) {
		return { ok: false, reason: (e as Error).message };
	}
	const allowed = new Set(allow.allowed.map((s) => s.toUpperCase()));
	const refused = new Set(allow.refused.map((s) => s.toUpperCase()));
	const failed: string[] = [];
	const walk = (node: Spdx): boolean => {
		if (node.op === 'or' || node.op === 'and') {
			// Walk every branch, so the error names every failing identifier.
			const results = node.items.map(walk);
			return node.op === 'or' ? results.some(Boolean) : results.every(Boolean);
		}
		if (node.op === 'with' && refused.has(node.exception.toUpperCase())) {
			failed.push(`${node.id} WITH ${node.exception} (refused exception)`);
			return false;
		}
		const ident = DEPRECATED[node.id] ?? node.id;
		const key = ident.toUpperCase();
		if (allowed.has(key)) return true;
		const name = ident === node.id ? ident : `${node.id} (read as ${ident})`;
		failed.push(refused.has(key) ? `${name} (refused)` : name);
		return false;
	};
	return walk(tree)
		? { ok: true, reason: '' }
		: { ok: false, reason: `not allowed: ${[...new Set(failed)].join(', ')}` };
}

// --- packages in the module graph ---------------------------------------------------------------

const toPosix = (p: string) => p.replace(/\\/g, '/');

/** The folder of a package a virtual module comes from: the project's own copy, else the one
 * this plugin runs beside (Vite's helpers come from the Vite that builds). */
export function locatePackage(root: string): (name: string) => string {
	const require = createRequire(import.meta.url);
	return (name) => {
		const own = `${toPosix(root)}/node_modules/${name}`;
		if (existsSync(`${own}/package.json`)) return own;
		try {
			return toPosix(dirname(require.resolve(`${name}/package.json`)));
		} catch {
			return own;
		}
	};
}

export interface PackageRef {
	name: string;
	/** The package folder, absolute. */
	dir: string;
}

/** Virtual modules whose code comes from a package: `\0vite/preload-helper.js` is Vite's,
 * `~icons/ph/x.svelte` is compiled by unplugin-icons from `@iconify-json/ph`. */
const VIRTUAL: [RegExp, (m: RegExpMatchArray) => string][] = [
	[/^\0?vite\//, () => 'vite'],
	[/^\0?rolldown\//, () => 'rolldown'],
	[/^\/?~icons\/([a-z0-9-]+)\//, (m) => `@iconify-json/${m[1]}`]
];

/** The npm package a module id belongs to, or null for Lodestar's own code. */
export function packageOf(
	id: string,
	root: string,
	locate: (name: string) => string = (name) => `${toPosix(root)}/node_modules/${name}`
): PackageRef | null {
	const clean = id.replace(/[?#].*$/, '');
	const path = clean.startsWith('\0') ? clean.slice(1) : clean;
	const norm = path.replace(/\\/g, '/');
	const at = norm.lastIndexOf('/node_modules/');
	if (at !== -1) {
		const rest = norm.slice(at + '/node_modules/'.length).split('/');
		const name = rest[0]?.startsWith('@') ? `${rest[0]}/${rest[1]}` : rest[0];
		if (!name) return null;
		return { name, dir: `${norm.slice(0, at)}/node_modules/${name}` };
	}
	for (const [re, pkg] of VIRTUAL) {
		const m = clean.match(re);
		if (m) {
			const name = pkg(m);
			return { name, dir: locate(name) };
		}
	}
	return null;
}

/** Bundled packages by folder, sorted by name then folder. */
export function bundledPackages(
	ids: Iterable<string>,
	root: string,
	locate?: (name: string) => string
): PackageRef[] {
	const found = new Map<string, PackageRef>();
	for (const id of ids) {
		const ref = packageOf(id, root, locate);
		if (ref && !found.has(ref.dir)) found.set(ref.dir, ref);
	}
	return [...found.values()].sort((a, b) =>
		a.name === b.name ? a.dir.localeCompare(b.dir) : a.name < b.name ? -1 : 1
	);
}

// --- the document -------------------------------------------------------------------------------

/** Licence, notice and copyright files at the top of a package folder. */
const TEXT_FILE = /^(licen[cs]e|copying|notice|copyright|unlicen[cs]e)/i;
const NOT_TEXT = /\.(js|cjs|mjs|ts|d\.ts|json|map|css|svelte)$/i;

export interface Fs {
	exists(path: string): boolean;
	read(path: string): string;
	/** Regular files (not folders or symlinks) directly in `dir`. */
	files(dir: string): string[];
	/** A regular file, not a symlink or anything else. */
	isFile(path: string): boolean;
	/** The path with every symlink resolved. */
	realpath(path: string): string;
}

export const nodeFs: Fs = {
	exists: existsSync,
	read: (p) => readFileSync(p, 'utf8'),
	files: (dir) => {
		try {
			return readdirSync(dir).filter((f) => lstatSync(join(dir, f)).isFile());
		} catch {
			return [];
		}
	},
	isFile: (p) => {
		try {
			return lstatSync(p).isFile();
		} catch {
			return false;
		}
	},
	realpath: (p) => realpathSync(p).replace(/\\/g, '/')
};

export interface LockEntry {
	version?: string;
	license?: string;
	dev?: boolean;
}

interface Override {
	ecosystem: string;
	name: string;
	version?: string;
	license?: string;
	file?: string;
	note?: string;
}

interface Extra {
	name: string;
	package: string;
	license: string;
	note?: string;
}

interface TextOverride {
	ecosystem: string;
	name: string;
	version?: string;
	/** A reviewed text, relative to web/ (kept under web/licenses/texts/). */
	file?: string;
	note?: string;
}

export interface Overrides {
	missing?: Override[];
	texts?: TextOverride[];
	extra?: Extra[];
	notices?: unknown[];
}

interface Texts {
	[file: string]: string;
}

export interface NpmEntry {
	name: string;
	version: string;
	license: string;
	repository: string;
	texts: Texts;
	/** `override`: the licence comes from overrides.json `missing`. */
	license_source?: 'override';
	/** The texts come from a reviewed file (overrides.json `texts`), not the package. */
	texts_from?: string;
	/** Shipped outside the module graph (overrides.json `extra`). */
	extra?: { component: string; note: string };
}

export interface RustDoc {
	targets: string[];
	lodestar: { license: string; crates: string[]; texts: Texts };
	notices: { name: string; file: string; texts: Texts }[];
	crates: unknown[];
	texts: Record<string, string>;
}

export interface Credit {
	name: string;
	license: string;
	source: string;
}

export interface LicensesDoc {
	schema: 1;
	lodestar: RustDoc['lodestar'];
	notices: RustDoc['notices'];
	npm: NpmEntry[];
	rust: { targets: string[]; crates: unknown[] };
	/** Palette credits from themes/*.json: credits, not dependencies, so not allowlist-checked. */
	credits: Credit[];
	/** Every licence text once, by SHA-256. */
	texts: Record<string, string>;
}

export interface Inputs {
	root: string;
	packages: PackageRef[];
	lock: Record<string, LockEntry>;
	rust: RustDoc;
	overrides: Overrides;
	allow: AllowList;
	credits: Credit[];
	fs?: Fs;
}

export const sha256 = (text: string) => createHash('sha256').update(text, 'utf8').digest('hex');

/** The package's repository (or homepage) as an http(s) URL, else empty: the page links to it.
 * npm's `owner/repo` and `github:owner/repo` shorthands mean GitHub; anything else that isn't
 * http(s) (`javascript:`, `git@…`, `gitlab:…`) is dropped. */
export function repositoryOf(pkg: Record<string, unknown>): string {
	const repo = pkg.repository;
	const url = typeof repo === 'string' ? repo : (repo as { url?: string } | undefined)?.url;
	const cleaned = (url ?? (typeof pkg.homepage === 'string' ? pkg.homepage : ''))
		.trim()
		.replace(/^git\+/, '')
		.replace(/\.git$/, '');
	const github = /^(?:github:)?([\w.-]+\/[\w.-]+)$/.exec(cleaned);
	if (github) return `https://github.com/${github[1]}`;
	return /^https?:\/\/\S+$/i.test(cleaned) ? cleaned : '';
}

/** Build the licences document and list every problem (a missing or disallowed licence, a stale
 * override). The caller fails the build when there are any. */
export function buildDocument(input: Inputs): {
	doc: LicensesDoc;
	problems: string[];
	warnings: string[];
} {
	const fs = input.fs ?? nodeFs;
	const problems: string[] = [];
	const warnings: string[] = [];
	const texts: Record<string, string> = { ...input.rust.texts };
	const missing = new Map(
		(input.overrides.missing ?? []).filter((o) => o.ecosystem === 'npm').map((o) => [o.name, o])
	);
	const used = new Set<string>();
	const root = input.root.replace(/\\/g, '/');

	/** Licence texts in a package folder. Only regular files inside it are read: a name that
	 * leaves it, or a symlink, is reported, so nothing else reaches the public asset. */
	const store = (dir: string, label: string, extraFile?: string): Texts => {
		const names = fs.files(dir).filter((f) => TEXT_FILE.test(f) && !NOT_TEXT.test(f));
		if (extraFile && !names.includes(extraFile)) names.push(extraFile);
		const out: Texts = {};
		const refuse = (name: string, why: string) =>
			problems.push(`${label}: licence file '${name}' ${why}; not read`);
		for (const name of names.sort()) {
			if (posix.isAbsolute(name) || /^[a-z]:/i.test(name) || /(^|[\\/])\.\.([\\/]|$)/.test(name)) {
				refuse(name, 'is outside the package folder');
				continue;
			}
			const path = `${dir}/${name}`;
			if (!fs.exists(path)) continue;
			if (!fs.isFile(path)) {
				refuse(name, 'is a symlink or not a regular file');
				continue;
			}
			const base = fs.realpath(dir);
			if (!fs.realpath(path).startsWith(`${base}/`)) {
				refuse(name, 'is outside the package folder');
				continue;
			}
			const text = fs.read(path).replace(/\r\n/g, '\n');
			const h = sha256(text);
			texts[h] = text;
			out[name] = h;
		}
		return out;
	};

	const textOverrides = new Map(
		(input.overrides.texts ?? []).filter((o) => o.ecosystem === 'npm').map((o) => [o.name, o])
	);
	const usedTexts = new Set<string>();
	/** The package's own texts, else a reviewed override's; none at all is an error. */
	const textsFor = (entry: NpmEntry, found: Texts) => {
		const label = `${entry.name} ${entry.version}`;
		const o = textOverrides.get(entry.name);
		entry.texts = found;
		if (o) {
			usedTexts.add(entry.name);
			if (!o.version) problems.push(`${label}: overrides.json \`texts\` entry has no \`version\``);
			else if (o.version !== entry.version)
				problems.push(
					`${label}: overrides.json \`texts\` was reviewed for ${o.version}; check the new version's files and update it`
				);
			if (Object.keys(found).length)
				problems.push(
					`${label}: now ships its own licence text; drop its overrides.json \`texts\` entry`
				);
			else if (o.file && fs.exists(`${root}/${o.file}`)) {
				const text = fs.read(`${root}/${o.file}`).replace(/\r\n/g, '\n');
				const h = sha256(text);
				texts[h] = text;
				entry.texts = { [posix.basename(o.file)]: h };
				entry.texts_from = `web/${o.file}`;
			} else
				problems.push(
					`${label}: overrides.json \`texts\` file ${o.file ? `web/${o.file} not found` : 'missing'}`
				);
		}
		if (!Object.keys(entry.texts).length)
			problems.push(
				`${label}: ships no licence text; add a reviewed \`texts\` entry with a \`file\` under web/licenses/texts/ to web/licenses/overrides.json`
			);
	};

	const readPackage = (dir: string): Record<string, unknown> | null => {
		const path = `${dir}/package.json`;
		return fs.exists(path) ? (JSON.parse(fs.read(path)) as Record<string, unknown>) : null;
	};

	const entryFor = (ref: PackageRef): NpmEntry | null => {
		const pkg = readPackage(ref.dir);
		if (!pkg) {
			problems.push(
				`${ref.name}: bundled, but ${posix.relative(root, ref.dir)}/package.json is missing`
			);
			return null;
		}
		const lockKey = posix.relative(root, ref.dir);
		const lock = input.lock[lockKey];
		const version = String(pkg.version ?? lock?.version ?? '');
		let license =
			lock?.license ??
			(typeof pkg.license === 'string' ? pkg.license : undefined) ??
			(Array.isArray(pkg.licenses)
				? (pkg.licenses as { type?: string }[]).map((l) => l.type).join(' OR ')
				: undefined);
		const entry: NpmEntry = {
			name: ref.name,
			version,
			license: '',
			repository: repositoryOf(pkg),
			texts: {}
		};
		const override = missing.get(ref.name);
		if (override) {
			used.add(ref.name);
			if (!override.version)
				problems.push(
					`${ref.name} ${version}: overrides.json \`missing\` entry has no \`version\``
				);
			else if (override.version !== version)
				problems.push(
					`${ref.name} ${version}: overrides.json \`missing\` was reviewed for ${override.version}; check the new version's licence and update it`
				);
			if (license)
				problems.push(
					`${ref.name} ${version}: has licence '${license}'; its override in overrides.json is no longer needed`
				);
			if (override.file && !fs.exists(`${ref.dir}/${override.file}`))
				problems.push(
					`${ref.name} ${version}: overrides.json says it ships ${override.file}, which is missing`
				);
			license = override.license;
			entry.license_source = 'override';
		}
		textsFor(entry, store(ref.dir, `${ref.name} ${version}`, override?.file));
		if (!license) {
			problems.push(
				`${ref.name} ${version}: no licence field; add a reviewed \`missing\` entry to web/licenses/overrides.json`
			);
		} else {
			const verdict = evaluate(license, input.allow);
			if (!verdict.ok)
				problems.push(`${ref.name} ${version}: licence '${license}' ${verdict.reason}`);
		}
		entry.license = license ?? '';
		return entry;
	};

	const npm: NpmEntry[] = [];
	for (const ref of input.packages) {
		const entry = entryFor(ref);
		if (entry) npm.push(entry);
	}
	// Reviewed ahead of use (svelte-toolbelt ships once a route uses bits-ui), so not an error.
	for (const name of [...missing.keys()].filter((n) => !used.has(n)).sort())
		warnings.push(
			`overrides.json \`missing\` names ${name}, which the client build doesn't bundle (yet)`
		);

	for (const extra of input.overrides.extra ?? []) {
		const dir = `${root}/node_modules/${extra.package}`;
		const pkg = readPackage(dir);
		if (!pkg) {
			problems.push(
				`overrides.json \`extra\` ${extra.name}: package ${extra.package} isn't installed`
			);
			continue;
		}
		const declared = typeof pkg.license === 'string' ? pkg.license : undefined;
		if (declared !== extra.license)
			problems.push(
				`overrides.json \`extra\` ${extra.name}: ${extra.package} now says ${declared ? `'${declared}'` : 'no licence'}, reviewed as '${extra.license}'`
			);
		const verdict = evaluate(extra.license, input.allow);
		if (!verdict.ok)
			problems.push(
				`${extra.name} (${extra.package}): licence '${extra.license}' ${verdict.reason}`
			);
		const entry: NpmEntry = {
			name: extra.package,
			version: String(pkg.version ?? ''),
			license: extra.license,
			repository: repositoryOf(pkg),
			texts: {},
			extra: { component: extra.name, note: extra.note ?? '' }
		};
		textsFor(entry, store(dir, `${entry.name} ${entry.version}`));
		npm.push(entry);
	}

	for (const name of [...textOverrides.keys()].filter((n) => !usedTexts.has(n)).sort())
		warnings.push(
			`overrides.json \`texts\` names ${name}, which the client build doesn't bundle (yet)`
		);

	npm.sort((a, b) =>
		a.name === b.name
			? (a.extra?.component ?? '').localeCompare(b.extra?.component ?? '')
			: a.name < b.name
				? -1
				: 1
	);
	const doc: LicensesDoc = {
		schema: 1,
		lodestar: input.rust.lodestar,
		notices: input.rust.notices,
		npm,
		rust: { targets: input.rust.targets, crates: input.rust.crates },
		credits: input.credits,
		texts: Object.fromEntries(Object.entries(texts).sort(([a], [b]) => (a < b ? -1 : 1)))
	};
	return { doc, problems, warnings };
}

/** Palette credits: each theme family's `license` and `source`, where it has them. */
export function themeCredits(root: string, fs: Fs = nodeFs): Credit[] {
	const dir = `${root}/themes`;
	return fs
		.files(dir)
		.filter((f) => f.endsWith('.json'))
		.sort()
		.flatMap((f) => {
			const t = JSON.parse(fs.read(`${dir}/${f}`)) as Record<string, unknown>;
			return typeof t.license === 'string'
				? [{ name: String(t.name ?? f), license: t.license, source: String(t.source ?? '') }]
				: [];
		});
}

/** Everything the document is built from, apart from the module graph. */
export function readInputs(root: string, packages: PackageRef[], fs: Fs = nodeFs): Inputs {
	const json = <T>(path: string) => JSON.parse(fs.read(path)) as T;
	const lock = json<{ packages: Record<string, LockEntry> }>(`${root}/package-lock.json`);
	return {
		root,
		packages,
		lock: lock.packages,
		rust: json<RustDoc>(`${root}/licenses/rust.json`),
		overrides: json<Overrides>(`${root}/licenses/overrides.json`),
		allow: json<AllowList>(`${root}/licenses/allow.json`),
		credits: themeCredits(root, fs),
		fs
	};
}

/** The emitted file name: the build's asset folder, `licenses-<hash>.json`. */
export function assetFileName(source: string, assetsDir: string): string {
	return posix.join(assetsDir, `licenses-${sha256(source).slice(0, 12)}.json`);
}

/** The asset folder: the folder of a string `assetFileNames` pattern (SvelteKit sets
 * `_app/immutable/assets/[name].[hash][extname]`), else Vite's `build.assetsDir`. */
function assetsDirOf(config: ResolvedConfig): string {
	const output = config.build.rollupOptions?.output;
	const first = Array.isArray(output) ? output[0] : output;
	const pattern = first?.assetFileNames;
	return typeof pattern === 'string' && pattern.includes('/')
		? dirname(pattern)
		: config.build.assetsDir;
}

// --- the plugin ---------------------------------------------------------------------------------

export function licenses(options: { root?: string } = {}): Plugin {
	let config: ResolvedConfig;
	let root = '';
	let clientBuild = false;
	let ref: string | undefined;

	const document = (ids: Iterable<string>) => {
		const { doc, problems, warnings } = buildDocument(
			readInputs(root, bundledPackages(ids, root, locatePackage(root)))
		);
		return { source: JSON.stringify(doc), problems, warnings };
	};
	const report = (problems: string[]) =>
		`Shipped-licence check failed (owner's decision §2.12):\n  - ${problems.join('\n  - ')}`;

	return {
		name: 'lodestar:licenses',
		configResolved(resolved) {
			config = resolved;
			root = (options.root ?? resolved.root).replace(/\\/g, '/');
			// SvelteKit builds the server first, then the client; only the client's graph ships.
			clientBuild = resolved.command === 'build' && !resolved.build.ssr;
		},
		resolveId(id) {
			return id === VIRTUAL_ID ? RESOLVED_ID : null;
		},
		load(id) {
			if (id !== RESOLVED_ID) return null;
			if (clientBuild) return `export const licensesUrl = ${JSON.stringify(PLACEHOLDER)};`;
			if (config.command === 'serve')
				return `export const licensesUrl = ${JSON.stringify(config.base + DEV_PATH)};`;
			// The server build never runs it (the app is client-only, `ssr = false`).
			return 'export const licensesUrl = "";';
		},
		renderStart() {
			if (!clientBuild) return;
			const { source, problems, warnings } = document(this.getModuleIds());
			for (const w of warnings) this.warn(w);
			if (problems.length) this.error(report(problems));
			ref = this.emitFile({
				type: 'asset',
				fileName: assetFileName(source, assetsDirOf(config)),
				source
			});
		},
		renderChunk: {
			// Before vite:asset's renderChunk, which turns the __VITE_ASSET__ marker into a URL
			// relative to the base, as for any other emitted asset.
			order: 'pre',
			handler(code) {
				if (!clientBuild || !ref || !code.includes(PLACEHOLDER)) return null;
				return { code: code.replaceAll(PLACEHOLDER, `__VITE_ASSET__${ref}__`), map: null };
			}
		},
		configureServer(server) {
			server.middlewares.use((req, res, next) => {
				if (req.url?.split('?')[0] !== config.base + DEV_PATH) return next();
				const ids = [...(server.environments.client?.moduleGraph.idToModuleMap.keys() ?? [])];
				const { source, problems } = document(ids);
				if (problems.length) server.config.logger.warn(report(problems));
				res.setHeader('Content-Type', 'application/json');
				res.end(source);
			});
		}
	};
}
