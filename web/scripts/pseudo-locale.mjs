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
	existsSync,
	mkdirSync,
	lstatSync,
	readdirSync,
	readFileSync,
	realpathSync,
	statSync,
	rmSync,
	symlinkSync,
	writeFileSync
} from 'node:fs';
import { tmpdir, userInfo } from 'node:os';
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
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
/** @param {string} text @returns {string} */
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
/** @param {Record<string, unknown>} messages @returns {Record<string, unknown>} */
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
// Hard-coded English: what the `pseudo` Playwright project may call "not from the catalogue".
//
// Every letter of a catalogue message is accented, so a run of plain Latin letters on a screen is
// text that bypassed the catalogue. Some of it is right as it is. Each allowance below says why,
// and the ones that could hide a real miss are tied to where they apply (`when`): a word that is
// also a catalogue message ("Show", "Today") is allowed only where its source is not the
// catalogue, never everywhere.

/** Applies a rule's pattern only inside a ⟦…⟧ message (a value the app put into a sentence). */
/** @param {RegExp} re */
const inMessage = (re) => new RegExp(`(?<=${OPEN}[^${CLOSE}]*)(?:${re.source})`, re.flags);

const INTL_TIMES =
	/\b(in\s+)?\d[\d.,]*\s*(seconds?|minutes?|hours?|days?|weeks?|months?|years?)(\s+ago)?\b|\b(yesterday|tomorrow|today|now)\b|\b(Mon|Tue|Wed|Thu|Fri|Sat|Sun|Jan|Feb|Mar|Apr|Jun|Jul|Aug|Sep|Oct|Nov|Dec)\b|\b\d\s?[AP]M\b|\bUTC\b/gi;

/**
 * Rules for text that is right as it is. `when(ctx)` limits a rule to where it is true; `ctx` is
 * `{ route, tag, testid, time }`: the route's name, the element's tag and `data-testid`, and
 * whether the element is inside a `<time>`.
 */
/** @type {{ why: string, pattern: RegExp, when?: (ctx: Ctx) => boolean }[]} */
export const ALLOWED = [
	{
		why: 'addresses and environment variable names, which are literal',
		pattern: /https?:\/\/\S+|\b[A-Z][A-Z0-9]*(_[A-Z0-9]+)+\b/g
	},
	{ why: 'the product name, never translated', pattern: /Lodestar/g },
	{
		why: 'product names of the clients Lodestar talks to',
		pattern: /\b(qBittorrent|SABnzbd|NZBGet|bitmagnet)\b/g
	},
	{
		why: 'protocol and format names, which are not words',
		pattern:
			/\b(BEP|DHT|SHA|MSE|uTP|TCP|UDP|IPv[46]|URL|URI|RSS|API|JSON|CSV|NZB|SSE|TLS|HTTPS?|DNS)\b/g
	},
	{
		why: 'hexadecimal with a digit in it (info hashes, ids), which is not a word',
		pattern: /\b(?=[0-9a-f]*\d)[0-9a-f]{6,}\b/gi
	},
	{
		why: 'byte and time units are formatted by Intl, not by the catalogue',
		pattern: /\b(GiB|MiB|KiB|TiB|KB|MB|GB|TB|ms|sec|secs|min|hr|hrs)\b/g
	},
	{
		why: 'Intl formats a time element ("in 5 min", "3 days ago", "Fri, Oct 23, UTC")',
		pattern: INTL_TIMES,
		when: (ctx) => ctx.tag === 'time' || !!ctx.time
	},
	{
		why: 'Intl formats an absolute date and time ("Wed, Oct 21, 2026, 12:00 UTC"): a shape no word can hide in',
		pattern:
			/\b((Mon|Tue|Wed|Thu|Fri|Sat|Sun),\s+)?[A-Z][a-z]{2} \d{1,2}, \d{4}, \d{1,2}:\d{2}(\s?[AP]M)?(\s+UTC)?/g
	},
	{
		why: 'Intl formats an absolute date ("Nov 14, 2023"): the whole text, nothing else',
		pattern: /^(·\s*)?[A-Z][a-z]{2} \d{1,2}, \d{4}$/
	},
	{
		why: 'Intl formats a time the app puts into a sentence, so only inside a ⟦…⟧ message',
		pattern: inMessage(INTL_TIMES)
	},
	// Fixture data comes from the mocks in e2e/support/routes.ts. Each shape is as narrow as the
	// fixture and, for a word that is also a message, tied to its element: a button or label that
	// says "Show" or "Title 1" must still be caught.
	{
		why: 'fixture: the saved search and torrent name "(The) Lantern Keeper (releases)"',
		pattern: /\b(The )?Lantern Keeper( releases)?\b/g
	},
	{
		why: 'fixture: dotted release names (The.Lantern.Keeper.2160p, Show.S01.1080p.WEB-DL-GRP)',
		pattern: /\b[\w-]+(\.[\w-]+){2,}\b/g
	},
	{
		why: 'fixture: the torrent page names its torrent "Show" (heading and breadcrumb)',
		pattern: /^Show$/,
		when: (ctx) => ctx.route === 'torrent' && (ctx.tag === 'h1' || ctx.tag === 'span')
	},
	{
		why: 'fixture: Discover row titles "Title 1" to "Title 3"',
		pattern: /^Title \d+$/,
		when: (ctx) => /^discover-row-[0-9a-f]+-title$/.test(ctx.testid ?? '')
	}
];

/**
 * KNOWN GAPS: hard-coded English the check found that the app still has. Each is a real defect,
 * listed so the nightly run reports new ones; the follow-up slice named in the decisions file
 * removes them. Delete an entry when its fix lands (the check then guards it). They go first in
 * the list: a broader allowance would otherwise eat part of their text.
 */
/** @type {{ gap: string, pattern: RegExp, when?: (ctx: Ctx) => boolean }[]} */
export const KNOWN_GAPS = [
	{
		gap: 'relative-time callers do not pass the catalogue\'s "just now" (src/lib/format/relative.ts keeps an English fallback)',
		pattern: /\bjust\s+now\b/g
	},
	{
		gap: 'Insights annotations insert the raw setting value ("aggressive") into a translated sentence (Movement.svelte)',
		// Only inside a pseudo message, so the word is not hidden everywhere else.
		pattern: inMessage(/\baggressive\b/g)
	}
];

/**
 * True when `text` holds English that did not come from the catalogue: a run of two or more plain
 * Latin letters, or one plain letter touching a ⟦ ⟧ end mark ("⟦…⟧s": a suffix added in code).
 */
/** @typedef {{ route?: string, tag?: string, testid?: string | null, time?: boolean }} Ctx */
/** @param {string} text @param {Ctx} [ctx] @returns {boolean} */
export function plainEnglish(text, ctx = {}) {
	let rest = text;
	for (const rule of [...KNOWN_GAPS, ...ALLOWED]) {
		if (rule.when && !rule.when(ctx)) continue;
		// A fresh regex: a shared global one keeps its position between calls.
		rest = rest.replace(new RegExp(rule.pattern.source, rule.pattern.flags), ' ');
	}
	return (
		/[A-Za-z]{2,}/.test(rest) ||
		new RegExp(`[A-Za-z][${OPEN}${CLOSE}]|[${OPEN}${CLOSE}][A-Za-z]`).test(rest)
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

const MARKER = '.lodestar-pseudo-scratch';

/** `child` is `parent` or inside it. A name that merely starts with `..` (`..pseudo`) is inside. */
/** @param {string} parent @param {string} child */
function within(parent, child) {
	const rel = relative(parent, child);
	return rel === '' || (!isAbsolute(rel) && rel !== '..' && !rel.startsWith('..' + sep));
}

/** The real path of `path`, or of its nearest existing ancestor with the rest put back on. */
/** @param {string} path @returns {string} */
function realish(path) {
	let existing = resolve(path);
	const rest = [];
	while (!existsSync(existing)) {
		rest.unshift(basename(existing));
		const up = dirname(existing);
		if (up === existing) break;
		existing = up;
	}
	return join(realpathSync(existing), ...rest);
}

/**
 * The scratch folder, laid out like the repository (`web/` inside it, `brand/` beside it) because
 * the asset step reads ../brand and writes ../packaging. Refuses a folder inside the repository
 * AND one that holds it (`/tmp`, `$HOME`, `..`), judged on real paths so a symlink into the
 * repository is caught: the build deletes the folder first.
 */
export function scratchDir(env = process.env) {
	if (!env.PSEUDO_DIR) throw new Error('pseudo-locale: PSEUDO_DIR is not set; see newScratchDir');
	const dir = realish(env.PSEUDO_DIR);
	const repo = realpathSync(REPO);
	if (within(repo, dir))
		throw new Error(`pseudo-locale: ${dir} is inside the repository; the pseudo build must not be`);
	if (within(dir, repo))
		throw new Error(`pseudo-locale: ${dir} contains the repository; refusing to build there`);
	return dir;
}

/**
 * The folder a run builds in: PSEUDO_DIR when set, otherwise one folder per user and port in the
 * temp folder (a Playwright run ends its server with a kill, so a random folder per run would
 * pile up). Not a name anyone else can have made first: an existing folder must be a real
 * folder, not a link, owned by this user, or the run refuses. Validated like any other.
 */
export function newScratchDir(env = process.env, port = 4174) {
	if (env.PSEUDO_DIR) return scratchDir(env);
	const user = String(process.getuid?.() ?? userInfo().username).replace(/[^A-Za-z0-9_.-]/g, '_');
	const dir = join(tmpdir(), `lodestar-pseudo-${user}-${port}`);
	if (existsSync(dir)) {
		const info = lstatSync(dir);
		const owner = process.getuid?.();
		if (!info.isDirectory() || (owner !== undefined && info.uid !== owner))
			throw new Error(`pseudo-locale: ${dir} exists and is not this user's own folder`);
	}
	return scratchDir({ ...env, PSEUDO_DIR: dir });
}

/** True when `dir` is absent, empty, or was made by this script (it carries the marker file). */
/** @param {string} dir @returns {boolean} */
export function safeToReplace(dir) {
	if (!existsSync(dir)) return true;
	if (existsSync(join(dir, MARKER))) return true;
	return readdirSync(dir).length === 0;
}

/** @param {string} cmd @param {string[]} args @param {string} cwd @param {NodeJS.ProcessEnv} [env] */
function run(cmd, args, cwd, env = process.env) {
	const result = spawnSync(cmd, args, {
		cwd,
		env,
		stdio: 'inherit',
		shell: process.platform === 'win32'
	});
	if (result.status !== 0) throw new Error(`pseudo-locale: ${cmd} ${args.join(' ')} failed`);
}

/**
 * Every `messages/en.json` under `dir`.
 * @param {string} dir
 * @returns {string[]}
 */
function catalogueFiles(dir) {
	return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
		const path = join(dir, entry.name);
		if (entry.isDirectory()) return catalogueFiles(path);
		return entry.name === 'en.json' && basename(dirname(path)) === 'messages' ? [path] : [];
	});
}

/**
 * The scratch copy's own `node_modules`: every package linked from the real one, except the
 * caches, which stay the scratch copy's own. (The asset step keeps a stamp in node_modules/.cache
 * that names the files it wrote; sharing it would make a later real `npm run assets` skip.)
 */
/** @param {string} dir */
function linkNodeModules(dir) {
	const own = new Set(['.cache', '.vite', '.vite-temp']);
	const target = join(dir, 'node_modules');
	mkdirSync(target);
	for (const name of readdirSync(join(WEB, 'node_modules'))) {
		if (own.has(name)) continue;
		const real = join(WEB, 'node_modules', name);
		// A junction is for a folder; a file is linked as a file (a junction on a file fails on Windows).
		symlinkSync(real, join(target, name), statSync(real).isDirectory() ? 'junction' : 'file');
	}
}

export function build(root = scratchDir()) {
	if (!safeToReplace(root))
		throw new Error(`pseudo-locale: ${root} is not empty and was not made by this script`);
	rmSync(root, { recursive: true, force: true });
	mkdirSync(root, { recursive: true, mode: 0o700 });
	writeFileSync(join(root, MARKER), 'Scratch build of the pseudo-locale check; safe to delete.\n');
	const dir = join(root, 'web');
	// The brand pack is only read (verified against its checksums), so the real one is linked.
	symlinkSync(join(REPO, 'brand'), join(root, 'brand'), 'junction');
	cpSync(WEB, dir, {
		recursive: true,
		filter: (src) => {
			const rel = relative(WEB, src);
			return rel === '' || !SKIP.has(rel.split(/[\\/]/)[0]);
		}
	});
	linkNodeModules(dir);
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
	const usage = 'usage: node scripts/pseudo-locale.mjs run [--port 4174]';
	if (command !== 'run') {
		console.error(usage);
		process.exit(2);
	}
	const at = rest.indexOf('--port');
	const port = at >= 0 ? Number(rest[at + 1]) : 4174;
	if (!Number.isInteger(port) || port < 1 || port > 65535) {
		console.error(`pseudo-locale: --port needs a port number (1 to 65535)\n${usage}`);
		process.exit(2);
	}
	const root = newScratchDir(process.env, port);
	const cleanUp = () => rmSync(root, { recursive: true, force: true });
	try {
		build(root);
	} catch (error) {
		cleanUp();
		throw error;
	}
	// No shell: the preview is the vite script run by this node, so killing it kills the server
	// (under `shell: true` on Windows the kill would only reach the shell).
	const vite = join(WEB, 'node_modules', 'vite', 'bin', 'vite.js');
	const server = spawn(
		process.execPath,
		[vite, 'preview', '--port', String(port), '--strictPort'],
		{
			cwd: join(root, 'web'),
			stdio: 'inherit'
		}
	);
	for (const signal of ['SIGINT', 'SIGTERM'])
		process.on(signal, () => server.kill(/** @type {NodeJS.Signals} */ (signal)));
	server.on('exit', (code) => {
		cleanUp();
		process.exit(code ?? 0);
	});
}
