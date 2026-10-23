// @vitest-environment node
// The licences plugin (scripts/licenses.ts): the SPDX evaluator, which packages a module graph
// ships, the document it builds, and a real Vite build that emits licenses-<hash>.json and
// resolves `virtual:licenses` to its URL.
import {
	mkdirSync,
	mkdtempSync,
	readFileSync,
	readdirSync,
	rmSync,
	symlinkSync,
	writeFileSync
} from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';
import {
	type Fs,
	type Inputs,
	type RustDoc,
	assetFileName,
	buildDocument,
	bundledPackages,
	evaluate,
	licenses,
	nodeFs,
	packageOf,
	parseSpdx,
	repositoryOf,
	sha256
} from './licenses.ts';

const allow = JSON.parse(readFileSync(new URL('../licenses/allow.json', import.meta.url), 'utf8'));

describe('allow.json', () => {
	it('is exactly the owner decision §2.12 list', () => {
		expect([...allow.allowed].sort()).toEqual(
			[
				'MIT',
				'MIT-0',
				'Apache-2.0',
				'BSD-2-Clause',
				'BSD-3-Clause',
				'ISC',
				'Zlib',
				'0BSD',
				'BSL-1.0',
				'BlueOak-1.0.0',
				'Unicode-3.0',
				'MPL-2.0',
				'CC0-1.0',
				'OFL-1.1',
				'GPL-3.0-only',
				'GPL-3.0-or-later',
				'AGPL-3.0-only',
				'AGPL-3.0-or-later'
			].sort()
		);
		expect(allow.refused).toEqual(expect.arrayContaining(['GPL-2.0-only', 'SSPL-1.0']));
	});
});

describe('SPDX evaluator', () => {
	const ok = (expr: string) =>
		expect(evaluate(expr, allow), expr).toEqual({ ok: true, reason: '' });
	const fails = (expr: string, mentions = '') => {
		const v = evaluate(expr, allow);
		expect(v.ok, expr).toBe(false);
		expect(v.reason).toContain(mentions);
	};

	it('reads single identifiers without regard to case', () => {
		ok('MIT');
		ok('mit');
		ok('AGPL-3.0-or-later');
		fails('WTFPL', 'WTFPL');
	});

	it('passes OR when any branch passes', () => {
		ok('MIT OR Apache-2.0');
		ok('GPL-2.0-only OR MIT');
		ok('MIT or Apache-2.0');
		fails('WTFPL OR GPL-2.0-only', 'GPL-2.0-only (refused)');
	});

	it('needs every AND branch', () => {
		ok('MIT AND BSD-3-Clause');
		fails('MIT AND WTFPL', 'WTFPL');
		fails('MIT AND GPL-2.0-only', 'GPL-2.0-only (refused)');
	});

	it('honours precedence and parentheses', () => {
		ok('MIT OR WTFPL AND Apache-2.0');
		ok('(Apache-2.0 OR MIT) AND BSD-3-Clause');
		fails('(MIT OR WTFPL) AND SSPL-1.0', 'SSPL-1.0 (refused)');
		fails('(MIT OR Apache-2.0', 'parentheses');
		fails('MIT OR Apache-2.0)', 'unexpected');
		fails('MIT OR', 'end');
		fails('', 'empty');
		expect(parseSpdx('A OR B AND (C WITH X)')).toEqual({
			op: 'or',
			items: [
				{ op: 'id', id: 'A' },
				{
					op: 'and',
					items: [
						{ op: 'id', id: 'B' },
						{ op: 'with', id: 'C', exception: 'X' }
					]
				}
			]
		});
	});

	it('passes WITH when its base does', () => {
		ok('Apache-2.0 WITH LLVM-exception');
		fails('GPL-2.0-only WITH Classpath-exception-2.0', 'refused');
		fails('MIT WITH', 'exception');
	});

	it('fails WITH when its exception is refused', () => {
		fails('Apache-2.0 WITH Commons-Clause', 'Commons-Clause (refused exception)');
		fails('MIT AND (Apache-2.0 WITH commons-clause)', 'refused exception');
		ok('Apache-2.0 WITH Commons-Clause OR MIT');
	});

	it('reads the slash form as OR', () => {
		ok('MIT/Apache-2.0');
		ok('Apache-2.0 / MIT / MPL-2.0');
		fails('WTFPL/SSPL-1.0', 'SSPL-1.0 (refused)');
	});

	it('reads deprecated ids as their current form', () => {
		ok('GPL-3.0');
		ok('GPL-3.0+');
		ok('AGPL-3.0');
		fails('GPL-2.0', 'GPL-2.0 (read as GPL-2.0-only) (refused)');
		fails('LGPL-2.1', 'LGPL-2.1-only');
	});

	it('refuses GPL-2.0-only', () => fails('GPL-2.0-only', 'GPL-2.0-only (refused)'));
});

const ROOT = '/w';

describe('packageOf', () => {
	it('finds the package of a node_modules path, scoped or nested, with queries', () => {
		expect(packageOf('/w/node_modules/svelte/src/index.js', ROOT)).toEqual({
			name: 'svelte',
			dir: '/w/node_modules/svelte'
		});
		expect(packageOf('/w/node_modules/@sveltejs/kit/src/runtime/client.js?v=1', ROOT)).toEqual({
			name: '@sveltejs/kit',
			dir: '/w/node_modules/@sveltejs/kit'
		});
		expect(packageOf('/w/node_modules/a/node_modules/b/x.css?inline', ROOT)).toEqual({
			name: 'b',
			dir: '/w/node_modules/a/node_modules/b'
		});
		expect(packageOf('C:\\w\\node_modules\\clsx\\dist\\clsx.mjs', 'C:\\w')?.name).toBe('clsx');
	});

	it('maps virtual modules to the package whose code they are', () => {
		expect(packageOf('\0vite/preload-helper.js', ROOT)?.dir).toBe('/w/node_modules/vite');
		expect(packageOf('~icons/ph/x-circle.svelte', ROOT)?.dir).toBe(
			'/w/node_modules/@iconify-json/ph'
		);
	});

	it("leaves Lodestar's own modules out", () => {
		expect(packageOf('/w/src/routes/+page.svelte', ROOT)).toBeNull();
		expect(packageOf('/w/src/app.css', ROOT)).toBeNull();
		expect(packageOf('/w/.svelte-kit/generated/root.js', ROOT)).toBeNull();
	});
});

/** An in-memory file system: path -> content. */
/** `links` maps a symlink's path to its target. */
function memFs(all: Record<string, string | undefined>, links: Record<string, string> = {}): Fs {
	// An undefined value deletes a default file.
	const files = Object.fromEntries(
		Object.entries(all).filter((e): e is [string, string] => e[1] !== undefined)
	);
	const real = (p: string) => links[p] ?? p;
	return {
		exists: (p) => real(p) in files,
		read: (p) => {
			if (!(real(p) in files)) throw new Error(`ENOENT ${p}`);
			return files[real(p)]!;
		},
		files: (dir) =>
			Object.keys(files)
				.filter((p) => dirname(p) === dir && !(p in links))
				.map((p) => p.slice(dir.length + 1)),
		isFile: (p) => p in files && !(p in links),
		realpath: real
	};
}

const MIT = 'MIT License\n\nCopyright (c) someone\n';
const rust: RustDoc = {
	targets: ['x86_64-unknown-linux-musl'],
	lodestar: { license: 'AGPL-3.0-or-later', crates: ['lodestar'], texts: { LICENSE: 'aa' } },
	notices: [],
	crates: [{ name: 'serde', license: 'MIT OR Apache-2.0', texts: { 'LICENSE-MIT': sha256(MIT) } }],
	texts: { [sha256(MIT)]: MIT, aa: 'AGPL text' }
};

function pkg(name: string, version: string, extra: Record<string, unknown> = {}) {
	return JSON.stringify({ name, version, ...extra });
}

function inputs(
	over: Partial<Inputs> = {},
	files: Record<string, string | undefined> = {},
	links: Record<string, string> = {}
): Inputs {
	const fs = memFs(
		{
			'/w/node_modules/svelte/package.json': pkg('svelte', '5.0.0', { license: 'MIT' }),
			'/w/node_modules/svelte/LICENSE.md': MIT,
			'/w/node_modules/bits-ui/package.json': pkg('bits-ui', '2.0.0', {
				license: 'MIT',
				repository: { url: 'git+https://github.com/huntabyte/bits-ui.git' }
			}),
			'/w/node_modules/bits-ui/LICENSE': 'bits licence\n',
			'/w/node_modules/bits-ui/license.js': 'export {}',
			'/w/node_modules/svelte-toolbelt/package.json': pkg('svelte-toolbelt', '0.10.6'),
			'/w/node_modules/svelte-toolbelt/LICENSE': MIT,
			'/w/node_modules/eslint/package.json': pkg('eslint', '9.0.0', { license: 'MIT' }),
			'/w/node_modules/daisyui/package.json': pkg('daisyui', '5.0.0', { license: 'MIT' }),
			'/w/node_modules/daisyui/LICENSE': 'daisy\n',
			...files
		},
		links
	);
	return {
		root: ROOT,
		packages: [],
		lock: {
			'node_modules/svelte': { version: '5.0.0', license: 'MIT', dev: true },
			'node_modules/bits-ui': { version: '2.0.0', license: 'MIT' },
			'node_modules/svelte-toolbelt': { version: '0.10.6' },
			'node_modules/eslint': { version: '9.0.0', license: 'MIT', dev: true }
		},
		rust,
		overrides: {},
		allow,
		credits: [],
		fs,
		...over
	};
}

/** A client build's module graph: app code, bundled packages (lazy chunk and CSS too). */
const GRAPH = [
	'/w/src/routes/+layout.svelte',
	'/w/src/app.css',
	'/w/node_modules/svelte/src/internal/client/index.js',
	'/w/node_modules/svelte/src/store/index.js',
	'/w/node_modules/bits-ui/dist/index.js',
	'/w/node_modules/bits-ui/dist/popover.css?direct',
	'/w/node_modules/svelte-toolbelt/dist/index.js'
];

describe('buildDocument', () => {
	const toolbelt = {
		ecosystem: 'npm',
		name: 'svelte-toolbelt',
		version: '0.10.6',
		license: 'MIT',
		file: 'LICENSE'
	};

	it('lists only bundled packages and keeps dev-only tools out', () => {
		const { doc, problems } = buildDocument(
			inputs({ packages: bundledPackages(GRAPH, ROOT), overrides: { missing: [toolbelt] } })
		);
		expect(problems).toEqual([]);
		expect(doc.npm.map((n) => `${n.name}@${n.version}`)).toEqual([
			'bits-ui@2.0.0',
			'svelte@5.0.0', // a devDependency, but bundled: it ships
			'svelte-toolbelt@0.10.6'
		]);
		expect(doc.npm.find((n) => n.name === 'eslint')).toBeUndefined();
		const bits = doc.npm[0]!;
		expect(bits.repository).toBe('https://github.com/huntabyte/bits-ui');
		expect(bits.texts).toEqual({ LICENSE: sha256('bits licence\n') });
	});

	it('stores each text once, shared with rust.json', () => {
		const { doc } = buildDocument(
			inputs({ packages: bundledPackages(GRAPH, ROOT), overrides: { missing: [toolbelt] } })
		);
		const h = sha256(MIT);
		expect(doc.npm.find((n) => n.name === 'svelte')!.texts).toEqual({ 'LICENSE.md': h });
		expect(doc.npm.find((n) => n.name === 'svelte-toolbelt')!.texts).toEqual({ LICENSE: h });
		expect(doc.texts[h]).toBe(MIT);
		expect(Object.keys(doc.texts)).toEqual(Object.keys(doc.texts).sort());
		expect(doc.rust.crates).toEqual(rust.crates);
		expect(doc.lodestar.license).toBe('AGPL-3.0-or-later');
	});

	it('fails a bundled package with no licence unless overridden', () => {
		const packages = bundledPackages(GRAPH, ROOT);
		const bare = buildDocument(inputs({ packages }));
		expect(bare.problems).toEqual([
			'svelte-toolbelt 0.10.6: no licence field; add a reviewed `missing` entry to web/licenses/overrides.json'
		]);
		const fixed = buildDocument(inputs({ packages, overrides: { missing: [toolbelt] } }));
		expect(fixed.problems).toEqual([]);
		const entry = fixed.doc.npm.find((n) => n.name === 'svelte-toolbelt')!;
		expect(entry.license).toBe('MIT');
		expect(entry.license_source).toBe('override');
		const stale = buildDocument(
			inputs({ packages, overrides: { missing: [{ ...toolbelt, version: '0.9.0' }] } })
		);
		expect(stale.problems[0]).toContain('reviewed for 0.9.0');
	});

	it('warns, without failing, on an override nothing bundles', () => {
		const { problems, warnings } = buildDocument(inputs({ overrides: { missing: [toolbelt] } }));
		expect(problems).toEqual([]);
		expect(warnings).toEqual([
			"overrides.json `missing` names svelte-toolbelt, which the client build doesn't bundle (yet)"
		]);
	});

	it('fails a disallowed licence, naming package and licence', () => {
		const { problems } = buildDocument(
			inputs(
				{
					packages: [{ name: 'svelte', dir: '/w/node_modules/svelte' }],
					lock: { 'node_modules/svelte': { version: '5.0.0', license: 'SSPL-1.0' } }
				},
				{}
			)
		);
		expect(problems).toEqual(["svelte 5.0.0: licence 'SSPL-1.0' not allowed: SSPL-1.0 (refused)"]);
	});

	it('fails a shipped package with no licence text unless a reviewed file supplies one', () => {
		const packages = [{ name: '@iconify-json/ph', dir: '/w/node_modules/@iconify-json/ph' }];
		const files = {
			'/w/node_modules/@iconify-json/ph/package.json': pkg('@iconify-json/ph', '1.2.2', {
				license: 'MIT'
			}),
			'/w/licenses/texts/phosphor-MIT.txt': 'MIT License\r\n\r\nCopyright (c) Phosphor Icons\r\n'
		};
		const bare = buildDocument(inputs({ packages }, files));
		expect(bare.problems).toEqual([
			'@iconify-json/ph 1.2.2: ships no licence text; add a reviewed `texts` entry with a `file` under web/licenses/texts/ to web/licenses/overrides.json'
		]);
		const text = {
			ecosystem: 'npm',
			name: '@iconify-json/ph',
			version: '1.2.2',
			file: 'licenses/texts/phosphor-MIT.txt'
		};
		const fixed = buildDocument(inputs({ packages, overrides: { texts: [text] } }, files));
		expect(fixed.problems).toEqual([]);
		const entry = fixed.doc.npm[0]!;
		const h = sha256('MIT License\n\nCopyright (c) Phosphor Icons\n');
		expect(entry.texts).toEqual({ 'phosphor-MIT.txt': h });
		expect(entry.texts_from).toBe('web/licenses/texts/phosphor-MIT.txt');
		expect(fixed.doc.texts[h]).toContain('Phosphor Icons');
		const gone = buildDocument(
			inputs(
				{ packages, overrides: { texts: [{ ...text, file: 'licenses/texts/nope.txt' }] } },
				files
			)
		).problems;
		expect(gone).toContain(
			'@iconify-json/ph 1.2.2: overrides.json `texts` file web/licenses/texts/nope.txt not found'
		);
		const shipsOwn = buildDocument(
			inputs(
				{ packages, overrides: { texts: [text] } },
				{ ...files, '/w/node_modules/@iconify-json/ph/LICENSE': 'own\n' }
			)
		).problems;
		expect(shipsOwn[0]).toContain('now ships its own licence text');
	});

	it('fails an extra with no licence text', () => {
		const extra = { name: 'daisyUI', package: 'daisyui', license: 'MIT' };
		const { problems } = buildDocument(
			inputs(
				{ overrides: { extra: [extra] } },
				{
					'/w/node_modules/daisyui/package.json': pkg('daisyui', '5.0.0', { license: 'MIT' }),
					'/w/node_modules/daisyui/LICENSE': undefined
				}
			)
		);
		expect(problems[0]).toContain('daisyui 5.0.0: ships no licence text');
	});

	it('reads only regular licence files inside the package folder', () => {
		const packages = bundledPackages(GRAPH, ROOT);
		const secret = '/home/me/.npmrc';
		const files = { [secret]: '//registry.npmjs.org/:_authToken=secret\n' };
		const escape = { ...toolbelt, file: '../../../home/me/.npmrc' };
		const out = buildDocument(inputs({ packages, overrides: { missing: [escape] } }, files));
		expect(out.problems).toContain(
			"svelte-toolbelt 0.10.6: licence file '../../../home/me/.npmrc' is outside the package folder; not read"
		);
		const absolute = { ...toolbelt, file: secret };
		expect(
			buildDocument(inputs({ packages, overrides: { missing: [absolute] } }, files)).problems
		).toContain(
			`svelte-toolbelt 0.10.6: licence file '${secret}' is outside the package folder; not read`
		);
		// A symlinked LICENSE, named by the override or found in the folder, is refused.
		const link = { '/w/node_modules/bits-ui/LICENSE-LINK': secret };
		const linked = buildDocument(
			inputs({ packages, overrides: { missing: [{ ...toolbelt, file: 'LICENSE-LINK' }] } }, files, {
				...link,
				'/w/node_modules/svelte-toolbelt/LICENSE-LINK': secret
			})
		);
		expect(linked.problems).toEqual([
			"svelte-toolbelt 0.10.6: licence file 'LICENSE-LINK' is a symlink or not a regular file; not read"
		]);
		expect(JSON.stringify(linked.doc)).not.toContain('secret');
		// A path that resolves outside through a symlinked folder is refused too.
		const viaDir = buildDocument(
			inputs(
				{ packages, overrides: { missing: [{ ...toolbelt, file: 'sub/LICENSE' }] } },
				{ ...files, '/w/node_modules/svelte-toolbelt/sub/LICENSE': 'x\n' },
				{ '/w/node_modules/svelte-toolbelt/sub/LICENSE': secret }
			)
		);
		expect(viaDir.problems[0]).toContain("'sub/LICENSE' is a symlink");
	});

	it('requires a version on every missing and file texts override', () => {
		const packages = bundledPackages(GRAPH, ROOT);
		const unpinned = { ...toolbelt, version: undefined };
		expect(
			buildDocument(inputs({ packages, overrides: { missing: [unpinned] } })).problems
		).toEqual(['svelte-toolbelt 0.10.6: overrides.json `missing` entry has no `version`']);
		const extra = { name: 'daisyUI', package: 'daisyui', license: 'MIT' };
		const text = { ecosystem: 'npm', name: 'daisyui', file: 'licenses/texts/daisy.txt' };
		const { problems } = buildDocument(
			inputs(
				{ overrides: { extra: [extra], texts: [text] } },
				{ '/w/node_modules/daisyui/LICENSE': undefined, '/w/licenses/texts/daisy.txt': 'd\n' }
			)
		);
		expect(problems).toEqual(['daisyui 5.0.0: overrides.json `texts` entry has no `version`']);
	});

	it('keeps only http(s) repository URLs', () => {
		expect(repositoryOf({ repository: 'javascript:alert(1)' })).toBe('');
		expect(repositoryOf({ repository: 'gitlab:x/y' })).toBe('');
		expect(repositoryOf({ repository: 'lukeed/clsx' })).toBe('https://github.com/lukeed/clsx');
		expect(repositoryOf({ repository: 'github:x/y' })).toBe('https://github.com/x/y');
		expect(repositoryOf({ repository: { url: 'git+ssh://git@github.com/x/y.git' } })).toBe('');
		expect(repositoryOf({ repository: { url: 'git+https://github.com/x/y.git' } })).toBe(
			'https://github.com/x/y'
		);
		expect(repositoryOf({ homepage: 'http://example.org/' })).toBe('http://example.org/');
		const packages = bundledPackages(GRAPH, ROOT);
		const { doc } = buildDocument(
			inputs(
				{ packages, overrides: { missing: [toolbelt] } },
				{
					'/w/node_modules/bits-ui/package.json': pkg('bits-ui', '2.0.0', {
						license: 'MIT',
						repository: 'javascript:alert(1)'
					})
				}
			)
		);
		expect(doc.npm[0]!.repository).toBe('');
	});

	it('flags an extra whose package declares no licence', () => {
		const extra = { name: 'daisyUI', package: 'daisyui', license: 'MIT' };
		const { problems } = buildDocument(
			inputs(
				{ overrides: { extra: [extra] } },
				{ '/w/node_modules/daisyui/package.json': pkg('daisyui', '5.0.0') }
			)
		);
		expect(problems).toEqual([
			"overrides.json `extra` daisyUI: daisyui now says no licence, reviewed as 'MIT'"
		]);
	});

	it('refuses real symlinks on disk', () => {
		const tmp = mkdtempSync(join(tmpdir(), 'licenses-fs-'));
		try {
			const pkgDir = join(tmp, 'node_modules', 'evil').replace(/\\/g, '/');
			mkdirSync(pkgDir, { recursive: true });
			writeFileSync(join(tmp, 'secret.txt'), 'token=secret\n');
			writeFileSync(join(pkgDir, 'package.json'), pkg('evil', '1.0.0', { license: 'MIT' }));
			writeFileSync(join(pkgDir, 'LICENSE'), 'MIT text\n');
			symlinkSync(join(tmp, 'secret.txt'), join(pkgDir, 'LICENSE-LINK'));
			expect(nodeFs.files(pkgDir).sort()).toEqual(['LICENSE', 'package.json']);
			expect(nodeFs.isFile(`${pkgDir}/LICENSE-LINK`)).toBe(false);
			expect(nodeFs.isFile(`${pkgDir}/LICENSE`)).toBe(true);
			const evil = { ecosystem: 'npm', name: 'evil', version: '1.0.0', file: 'LICENSE-LINK' };
			const { doc, problems } = buildDocument(
				inputs({
					root: tmp.replace(/\\/g, '/'),
					packages: [{ name: 'evil', dir: pkgDir }],
					overrides: { missing: [{ ...evil, license: 'MIT' }] },
					fs: nodeFs
				})
			);
			expect(problems).toContain(
				"evil 1.0.0: licence file 'LICENSE-LINK' is a symlink or not a regular file; not read"
			);
			expect(doc.npm[0]!.texts).toEqual({ LICENSE: sha256('MIT text\n') });
			expect(JSON.stringify(doc)).not.toContain('secret');
		} finally {
			rmSync(tmp, { recursive: true, force: true });
		}
	});

	it('adds reviewed extras that are not in the module graph', () => {
		const extra = { name: 'daisyUI', package: 'daisyui', license: 'MIT', note: 'CSS' };
		const { doc, problems } = buildDocument(inputs({ overrides: { extra: [extra] } }));
		expect(problems).toEqual([]);
		expect(doc.npm).toEqual([
			{
				name: 'daisyui',
				version: '5.0.0',
				license: 'MIT',
				repository: '',
				texts: { LICENSE: sha256('daisy\n') },
				extra: { component: 'daisyUI', note: 'CSS' }
			}
		]);
		const changed = buildDocument(
			inputs({ overrides: { extra: [{ ...extra, license: 'ISC' }] } })
		).problems;
		expect(changed[0]).toContain("now says 'MIT', reviewed as 'ISC'");
	});
});

describe('assetFileName', () => {
	it('is content-hashed in the asset folder', () => {
		expect(assetFileName('{}', '_app/immutable/assets')).toBe(
			`_app/immutable/assets/licenses-${sha256('{}').slice(0, 12)}.json`
		);
	});
});

describe('plugin in a real build', () => {
	const dirs: string[] = [];
	afterEach(() => dirs.splice(0).forEach((d) => rmSync(d, { recursive: true, force: true })));

	function project(): string {
		const root = mkdtempSync(join(tmpdir(), 'licenses-'));
		dirs.push(root);
		const write = (rel: string, text: string) => {
			mkdirSync(dirname(join(root, rel)), { recursive: true });
			writeFileSync(join(root, rel), text);
		};
		write('index.html', '<script type="module" src="/main.js"></script>');
		// The licences URL is only reached through a lazy chunk, as About will load it.
		write('main.js', "import('./about.js').then((m) => m.show());");
		write(
			'about.js',
			"import { licensesUrl } from 'virtual:licenses';\nimport { pad } from 'tiny-pad';\nexport const show = () => fetch(pad(licensesUrl));"
		);
		write('node_modules/tiny-pad/package.json', pkg('tiny-pad', '1.2.3', { main: 'index.js' }));
		write('node_modules/tiny-pad/index.js', 'export const pad = (s) => s + "";');
		write('node_modules/tiny-pad/LICENSE', MIT);
		write(
			'package-lock.json',
			JSON.stringify({
				packages: { 'node_modules/tiny-pad': { version: '1.2.3', license: 'MIT' } }
			})
		);
		write('licenses/rust.json', JSON.stringify(rust));
		write('licenses/overrides.json', '{}');
		write('licenses/allow.json', JSON.stringify(allow));
		write(
			'themes/okabe.json',
			JSON.stringify({ name: 'Okabe-Ito', license: 'credit', source: 'x' })
		);
		return root;
	}

	it('emits licenses-<hash>.json and points virtual:licenses at it', async () => {
		const { build } = await import('vite');
		const root = project();
		await build({
			root,
			configFile: false,
			logLevel: 'silent',
			base: './',
			plugins: [licenses()],
			build: { outDir: 'dist', assetsDir: 'assets', minify: false }
		});
		const assets = readdirSync(join(root, 'dist/assets'));
		const name = assets.find((f) => /^licenses-[0-9a-f]{12}\.json$/.test(f));
		expect(name, assets.join(', ')).toBeDefined();
		const doc = JSON.parse(readFileSync(join(root, 'dist/assets', name!), 'utf8'));
		// Vite's preload helper ships with the lazy import: it comes from the Vite that builds.
		expect(doc.npm.map((n: { name: string }) => n.name)).toEqual(['tiny-pad', 'vite']);
		expect(doc.credits).toEqual([{ name: 'Okabe-Ito', license: 'credit', source: 'x' }]);
		expect(assetFileName(readFileSync(join(root, 'dist/assets', name!), 'utf8'), 'assets')).toBe(
			`assets/${name}`
		);
		const js = assets
			.filter((f) => f.endsWith('.js'))
			.map((f) => readFileSync(join(root, 'dist/assets', f), 'utf8'))
			.join('\n');
		expect(js).not.toContain('__LODESTAR_LICENSES_URL__');
		expect(js).not.toContain('__VITE_ASSET__');
		expect(js).toContain(name);
	});

	it("goes through Vite's asset URLs, so SvelteKit's renderBuiltUrl applies", async () => {
		const { build } = await import('vite');
		const root = project();
		await build({
			root,
			configFile: false,
			logLevel: 'silent',
			plugins: [licenses()],
			// As SvelteKit does for the client: JS asset URLs computed at runtime.
			experimental: {
				renderBuiltUrl: (file, { hostType }) =>
					hostType === 'js' ? { runtime: `globalThis.__base(${JSON.stringify(file)})` } : undefined
			},
			build: { outDir: 'dist', assetsDir: 'assets', minify: false }
		});
		const assets = readdirSync(join(root, 'dist/assets'));
		const name = assets.find((f) => f.startsWith('licenses-'));
		const js = assets
			.filter((f) => f.endsWith('.js'))
			.map((f) => readFileSync(join(root, 'dist/assets', f), 'utf8'))
			.join('\n');
		expect(js).toContain(`globalThis.__base("assets/${name}")`);
	});

	it('fails the build on a disallowed licence', async () => {
		const { build } = await import('vite');
		const root = project();
		writeFileSync(
			join(root, 'package-lock.json'),
			JSON.stringify({
				packages: { 'node_modules/tiny-pad': { version: '1.2.3', license: 'GPL-2.0-only' } }
			})
		);
		await expect(
			build({
				root,
				configFile: false,
				logLevel: 'silent',
				plugins: [licenses()],
				build: { outDir: 'dist', write: false }
			})
		).rejects.toThrow(
			/tiny-pad 1\.2\.3: licence 'GPL-2\.0-only' not allowed: GPL-2\.0-only \(refused\)/
		);
	});
});
