// @vitest-environment node
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';
import { logos, scenes } from '../src/lib/brand/scenes.generated.ts';
import {
	AVIF_540_LIMIT_BYTES,
	PackVerificationError,
	assertSceneBudget,
	assertSceneSizes,
	compassFallbacks,
	buildCompass,
	buildFaviconSvg,
	encodeIco,
	fixManifest,
	sceneEntries,
	verifyPack
} from './assets.ts';

const source = new URL('../../brand/source/', import.meta.url).pathname;
const read = (rel: string) => readFileSync(join(source, rel), 'utf8');

const dirs: string[] = [];
afterEach(() => dirs.splice(0).forEach((d) => rmSync(d, { recursive: true, force: true })));
function pack(files: Record<string, string>, sums: Record<string, string> = files) {
	const dir = mkdtempSync(join(tmpdir(), 'assets-'));
	dirs.push(dir);
	for (const [name, text] of Object.entries(files)) writeFileSync(join(dir, name), text);
	const lines = Object.entries(sums).map(
		([name, text]) => `${createHash('sha256').update(text).digest('hex')}  ${name}`
	);
	writeFileSync(join(dir, 'SHA256SUMS.txt'), `${lines.join('\n')}\n`);
	return dir;
}

describe('verifyPack', () => {
	it('accepts the committed source pack', () => {
		expect(verifyPack(source).size).toBeGreaterThan(50);
	});

	it('accepts a matching pack', () => {
		expect(verifyPack(pack({ 'a.txt': 'one', 'b.txt': 'two' })).size).toBe(2);
	});

	it('fails on a checksum mismatch, naming the file', () => {
		const dir = pack({ 'a.txt': 'tampered' }, { 'a.txt': 'original' });
		expect(() => verifyPack(dir)).toThrow(PackVerificationError);
		expect(() => verifyPack(dir)).toThrow(/mismatch: a\.txt/);
	});

	it('fails on a file the sums list but the pack lacks', () => {
		const dir = pack({ 'a.txt': 'one' }, { 'a.txt': 'one', 'gone.txt': 'x' });
		expect(() => verifyPack(dir)).toThrow(/missing: gone\.txt/);
	});

	it('fails when SHA256SUMS.txt is absent or malformed', () => {
		const dir = pack({ 'a.txt': 'one' });
		writeFileSync(join(dir, 'SHA256SUMS.txt'), 'not a checksum line\n');
		expect(() => verifyPack(dir)).toThrow(/Malformed/);
		rmSync(join(dir, 'SHA256SUMS.txt'));
		expect(() => verifyPack(dir)).toThrow(/Missing/);
	});
});

describe('scene AVIF budget (45 KB at 540w)', () => {
	it('passes at the limit', () => {
		expect(() =>
			assertSceneBudget([{ file: 'a-540.avif', bytes: AVIF_540_LIMIT_BYTES }])
		).not.toThrow();
	});

	it('fails over the limit with a report listing every file and its size', () => {
		const files = [
			{ file: 'small-540.avif', bytes: 30_000 },
			{ file: 'big-540.avif', bytes: AVIF_540_LIMIT_BYTES + 1 }
		];
		expect(() => assertSceneBudget(files)).toThrow(/OVER 45001 B {2}big-540\.avif/);
		expect(() => assertSceneBudget(files)).toThrow(/ok {3}30000 B {2}small-540\.avif/);
	});
});

describe('scene metadata', () => {
	it('derives one entry per state and mode from the manifest, aliases included', () => {
		const manifest = JSON.parse(read('visuals/manifest.json'));
		const sizes = new Map<string, { width: number; height: number }>();
		for (const modes of Object.values<Record<string, string>>(manifest.stateAssets)) {
			for (const rel of Object.values(modes)) {
				sizes.set(/([^/]+)\.png$/.exec(rel)![1]!, { width: 724, height: 1086 });
			}
		}
		const entries = sceneEntries(manifest, sizes);
		expect(entries).toHaveLength(Object.keys(manifest.stateAssets).length * 2);
		const found = entries.find((e) => e.id === 'found_nodes' && e.mode === 'dark')!;
		const crawling = entries.find((e) => e.id === 'searching_crawling' && e.mode === 'dark')!;
		expect(found.srcset).toEqual(crawling.srcset);
		const idle = entries.find((e) => e.id === 'offline_idle' && e.mode === 'light')!;
		expect(idle.srcset.avif).toContain('welcome_onboarding_light-540.avif 540w');
	});

	it('gives every generated scene and logo a width and height', () => {
		expect(scenes.length).toBe(18);
		for (const s of scenes) {
			expect(s.width, s.id).toBeGreaterThan(0);
			expect(s.height, s.id).toBeGreaterThan(0);
			expect(s.srcset.avif).toMatch(/360w.*540w.*724w/);
			expect(s.srcset.webp).toMatch(/-540\.webp 540w$/);
		}
		expect(logos.length).toBe(6);
		for (const l of logos) {
			expect(l.width, l.id).toBeGreaterThan(0);
			expect(l.height, l.id).toBeGreaterThan(0);
		}
		expect(logos.find((l) => l.id === 'wordmark')!.width).toBe(240);
	});
});

describe('scene sizes', () => {
	const size = (width: number, height: number) => ({ width, height });
	it('accepts sources that share one size', () => {
		expect(() =>
			assertSceneSizes(
				new Map([
					['a', size(724, 1086)],
					['b', size(724, 1086)]
				])
			)
		).not.toThrow();
	});

	it('fails when a source differs, or is narrower than the largest variant', () => {
		expect(() =>
			assertSceneSizes(
				new Map([
					['a', size(724, 1086)],
					['b', size(724, 1000)]
				])
			)
		).toThrow(/share one size/);
		expect(() => assertSceneSizes(new Map([['a', size(600, 900)]]))).toThrow(/724px/);
		expect(() => assertSceneSizes(new Map())).toThrow();
	});

	it('measures the real sources (the committed entries match the PNGs)', async () => {
		const { default: sharp } = await import('sharp');
		const meta = await sharp(join(source, 'visuals/no_results_light.png')).metadata();
		const entry = scenes.find((s) => s.id === 'no_results' && s.mode === 'light')!;
		expect([entry.width, entry.height]).toEqual([meta.width, meta.height]);
	});
});

describe('compass', () => {
	const light = read('icons/compass_light.svg');
	const dark = read('icons/compass_dark.svg');
	const stripFallbacks = (svg: string) => svg.replace(/var\(--[\w-]+,\s*#[0-9a-fA-F]+\)/g, '');

	it('has no hex colour outside var() fallbacks', () => {
		for (const svg of [
			buildCompass(light, dark),
			readFileSync('src/lib/brand/compass.svg', 'utf8')
		]) {
			expect(stripFallbacks(svg)).not.toMatch(/#[0-9a-fA-F]{3,8}/);
			for (const role of ['surface', 'primary', 'accent', 'star']) {
				expect(svg).toContain(`var(--brand-${role},#`);
			}
		}
	});

	it('takes its fallbacks from the Riverstone theme and the token table', () => {
		const river = readFileSync('themes/riverstone.json', 'utf8');
		const fallbacks = compassFallbacks(river);
		const svg = readFileSync('src/lib/brand/compass.svg', 'utf8');
		const found = Object.fromEntries(
			[...svg.matchAll(/var\(--brand-(\w+),(#[0-9A-Fa-f]+)\)/g)].map((m) => [m[1], m[2]])
		);
		expect(found.surface).toBe('#FFFFFF');
		expect(found).toMatchObject(fallbacks);
		// design-tokens.md §4: every Riverstone-light fallback matches the documented table.
		const doc = readFileSync('../docs/design-tokens.md', 'utf8');
		for (const [role, hex] of Object.entries(found)) {
			expect(doc).toMatch(new RegExp(`\\| \`--brand-${role}\` \\|[^|]*\\| \`${hex}\``, 'i'));
		}
	});

	it('stays about half a kilobyte', () => {
		expect(Buffer.byteLength(buildCompass(light, dark))).toBeLessThan(700);
	});

	it('refuses a dark variant whose geometry differs', () => {
		expect(() => buildCompass(light, dark.replace('r="182"', 'r="150"'))).toThrow(
			/more than colour/
		);
	});

	it('builds a favicon that swaps palettes under prefers-color-scheme', () => {
		const svg = buildFaviconSvg(light, dark);
		expect(svg).toContain('@media(prefers-color-scheme:dark)');
		expect(svg).toContain('.s{fill:#F3F6F5}');
		expect(svg).toContain('.s{fill:#22313A}');
		expect(svg).not.toMatch(/(fill|stroke)="#/);
	});
});

describe('favicon manifest and ico', () => {
	it('rewrites icon paths to /brand/favicons/', () => {
		const out = JSON.parse(fixManifest(read('favicons/site.webmanifest')));
		expect(out.icons.map((i: { src: string }) => i.src)).toEqual([
			'/brand/favicons/android-chrome-192x192.png',
			'/brand/favicons/android-chrome-512x512.png'
		]);
	});

	it('encodes an ico directory with 256 stored as 0', () => {
		const ico = encodeIco([
			{ size: 16, png: Buffer.from('a') },
			{ size: 256, png: Buffer.from('bc') }
		]);
		expect(ico.readUInt16LE(2)).toBe(1);
		expect(ico.readUInt16LE(4)).toBe(2);
		expect(ico.readUInt8(6)).toBe(16);
		expect(ico.readUInt8(6 + 16)).toBe(0);
		expect(ico.readUInt32LE(6 + 16 + 12)).toBe(6 + 32 + 1);
		expect(ico.length).toBe(6 + 32 + 3);
	});
});
