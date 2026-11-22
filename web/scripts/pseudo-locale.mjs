// Pseudo-locale for the UI checks (docs/decisions/2026-11-22-X-ui-checks-3.md; review 02 §3c).
//
// Every message becomes a longer, accented look-alike wrapped in ⟦ ⟧: "Search" -> "⟦Śéàŕçĥ ŀöŕéɱ⟧".
// A screen built from it shows three things at a glance, which the `pseudo` Playwright project
// checks: text that grows by about a third still fits (no overflow, no clipping), the bracket
// ends show where text is cut, and any run of plain Latin letters is a string that bypassed the
// message catalogue (hard-coded English).
//
// Message-level, not a translation: placeholders such as {count} are kept exactly, so the app
// still formats them. Run only by the nightly and manual job (.github/workflows/e2e.yml) and by
// `npx playwright test --project=pseudo`; never by a PR run.
//
//   node scripts/pseudo-locale.mjs run [--port 4174]
//     Copies web/ to a scratch folder outside the repository, swaps in the pseudo catalogue,
//     builds there and serves the build with `vite preview`. web/messages and web/build are not
//     touched, and the scratch folder is never inside the repository, so no release, Docker or
//     packaging path can pick the pseudo build up.
import { spawn, spawnSync } from 'node:child_process';
import {
	cpSync,
	mkdirSync,
	readdirSync,
	readFileSync,
	rmSync,
	symlinkSync,
	writeFileSync
} from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, dirname, isAbsolute, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const OPEN = '⟦';
export const CLOSE = '⟧';

// Every ASCII letter has an accented look-alike, so no plain Latin letter survives in a pseudo
// message. That is what lets the check call a plain-letter run "not from the catalogue".
const FROM = 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ';
const TO = 'àƀçðéƒĝĥíĵķŀɱñöƥǫŕšţüṽŵẋýžÀƁÇÐÉƑĜĤÍĴĶĿṀÑÖƤǪŔŠŢÜṼŴẊÝŽ';
const MAP = new Map([...FROM].map((c, i) => [c, [...TO][i]]));

// Appended as padding, so a message grows by about EXPANSION; words so it can still wrap.
const PADDING = 'ŀöŕéɱ ïƥšüɱ ðöŀöŕ šïţ àɱéţ çöñšéçţéţüŕ ';
export const EXPANSION = 0.35;

/** One message: accented, padded by about a third, wrapped in ⟦ ⟧; {placeholders} untouched. */
export function pseudoText(text) {
	const parts = text.split(/(\{[^{}]*\})/); // odd indices are the placeholders
	const accented = parts
		.map((part, i) => (i % 2 ? part : [...part].map((c) => MAP.get(c) ?? c).join('')))
		.join('');
	const letters = [...text.replace(/\{[^{}]*\}/g, '')].length;
	const pad = Math.ceil(letters * EXPANSION);
	const padding =
		pad > 0 ? ' ' + PADDING.repeat(Math.ceil(pad / PADDING.length)).slice(0, pad) : '';
	return `${OPEN}${accented}${padding.trimEnd()}${CLOSE}`;
}

/** A whole catalogue (key -> message), keys unchanged. */
export function pseudoCatalogue(messages) {
	return Object.fromEntries(
		Object.entries(messages).map(([key, value]) => [
			key,
			// `$schema` and other non-message entries pass through.
			typeof value === 'string' && !key.startsWith('$') ? pseudoText(value) : value
		])
	);
}

// ---------------------------------------------------------------------------------------------
// The scratch build.

const WEB = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const REPO = resolve(WEB, '..');
/** Folders of web/ the scratch copy leaves out: generated, large, or not part of the build. */
const SKIP = new Set([
	'node_modules',
	'.svelte-kit',
	'build',
	'test-results',
	'playwright-report',
	'e2e',
	'messages'
]);

/**
 * The scratch folder, laid out like the repository (`web/` inside it, `brand/` beside it) because
 * the asset step reads ../brand and writes ../packaging. Refuses anywhere inside the repository.
 */
export function scratchDir(env = process.env) {
	const dir = resolve(env.PSEUDO_DIR || join(tmpdir(), 'lodestar-pseudo'));
	const rel = relative(REPO, dir);
	if (rel === '' || (!rel.startsWith('..') && !isAbsolute(rel)))
		throw new Error(`pseudo-locale: ${dir} is inside the repository; the pseudo build must not be`);
	return dir;
}

function run(cmd, args, cwd, env = process.env) {
	const result = spawnSync(cmd, args, {
		cwd,
		env,
		stdio: 'inherit',
		shell: process.platform === 'win32'
	});
	if (result.status !== 0) throw new Error(`pseudo-locale: ${cmd} ${args.join(' ')} failed`);
}

/** Every `messages/en.json` under `dir`. */
function catalogueFiles(dir) {
	return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
		const path = join(dir, entry.name);
		if (entry.isDirectory()) return catalogueFiles(path);
		return entry.name === 'en.json' && basename(dirname(path)) === 'messages' ? [path] : [];
	});
}

export function build(root = scratchDir()) {
	rmSync(root, { recursive: true, force: true });
	const dir = join(root, 'web');
	// The brand pack is only read (verified against its checksums), so the real one is linked.
	mkdirSync(root, { recursive: true });
	symlinkSync(join(REPO, 'brand'), join(root, 'brand'), 'junction');
	cpSync(WEB, dir, {
		recursive: true,
		filter: (src) => {
			const rel = relative(WEB, src);
			return rel === '' || !SKIP.has(rel.split(/[\\/]/)[0]);
		}
	});
	symlinkSync(join(WEB, 'node_modules'), join(dir, 'node_modules'), 'junction');
	// The per-feature source catalogues, made pseudo in the copy. vite.config.ts merges them into
	// messages/en.json at the start of every build, so the sources are what has to change.
	let files = 0;
	for (const file of catalogueFiles(join(dir, 'src', 'lib'))) {
		writeFileSync(
			file,
			JSON.stringify(pseudoCatalogue(JSON.parse(readFileSync(file, 'utf8'))), null, '\t') + '\n'
		);
		files++;
	}
	if (files === 0) throw new Error('pseudo-locale: no messages/en.json found under src/lib');
	for (const script of ['themes', 'assets']) run('npm', ['run', script], dir);
	run('npx', ['vite', 'build'], dir, { ...process.env, VITE_DEV_ROUTES: '1' });
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
	const [command, ...rest] = process.argv.slice(2);
	if (command !== 'run') {
		console.error('usage: node scripts/pseudo-locale.mjs run [--port 4174]');
		process.exit(2);
	}
	const at = rest.indexOf('--port');
	const port = String(at >= 0 ? rest[at + 1] : 4174);
	const root = scratchDir();
	build(root);
	const server = spawn('npx', ['vite', 'preview', '--port', port, '--strictPort'], {
		cwd: join(root, 'web'),
		stdio: 'inherit',
		shell: process.platform === 'win32'
	});
	for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => server.kill(signal));
	server.on('exit', (code) => process.exit(code ?? 0));
}
