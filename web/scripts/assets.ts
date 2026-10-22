// Brand asset pipeline (architecture §10.19). Run with `npm run assets`, which `prebuild` calls.
//
// Reads the original pack in brand/source/ (verified against SHA256SUMS.txt first) and writes:
//   web/static/brand/{scenes,logos,favicons}/   served assets, embedded in the binary
//   web/src/lib/brand/scenes.generated.ts       scene + logo metadata (width/height, srcset)
//   web/src/lib/brand/compass.svg               the inline compass mark (CSS-variable fills)
//   packaging/icons/                            Windows .ico and the Unraid PNG (used by M6)
//
// Work is skipped when the hash of the inputs (and of this script) has not changed.
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import sharp from 'sharp';
import { format, resolveConfig } from 'prettier';

const here = dirname(fileURLToPath(import.meta.url));

/** §10.19: scene widths, and the AVIF-at-540w ceiling (decimal KB, the stricter reading). */
export const SCENE_WIDTHS = [360, 540, 724] as const;
/** WebP is only the fallback for browsers without AVIF, so one width keeps the pack small. */
export const WEBP_SCENE_WIDTHS = [540] as const;
export const SCENE_AVIF = { quality: 32, effort: 4 } as const;
export const SCENE_WEBP = { quality: 28, effort: 6, smartSubsample: true } as const;
export const AVIF_540_LIMIT_BYTES = 45_000;
/** Intrinsic size of every scene PNG; srcset variants keep this aspect ratio. */
const SCENE_SOURCE = { width: 724, height: 1086 };

/** Logo display widths in CSS px (1x); the 2x variant doubles them. §10.19 fixes only the wordmark. */
export const LOGO_DISPLAY_WIDTH = { wordmark: 240, horizontal: 320, stacked: 200 } as const;
export type LogoKind = keyof typeof LOGO_DISPLAY_WIDTH;

const ICO_SIZES = [16, 24, 32, 48, 64, 128, 256];

export type Mode = 'light' | 'dark';
export interface Paths {
	/** brand/source */
	source: string;
	/** web/static/brand */
	staticOut: string;
	/** web/src/lib/brand */
	libOut: string;
	/** packaging/icons */
	packagingOut: string;
	/** where the input-hash stamp lives */
	cacheDir: string;
}

export const defaultPaths = (): Paths => ({
	source: resolve(here, '../../brand/source'),
	staticOut: resolve(here, '../static/brand'),
	libOut: resolve(here, '../src/lib/brand'),
	packagingOut: resolve(here, '../../packaging/icons'),
	cacheDir: resolve(here, '../node_modules/.cache/lodestar-assets')
});

// ---------------------------------------------------------------------------------------------
// Source pack verification

export class PackVerificationError extends Error {}

const sha256 = (data: Buffer | string) => createHash('sha256').update(data).digest('hex');

/** Parse `<hash>  <path>` lines (sha256sum format, `*` binary marker allowed). */
export function parseChecksums(text: string): Map<string, string> {
	const sums = new Map<string, string>();
	for (const line of text.split(/\r?\n/)) {
		if (!line.trim()) continue;
		const m = /^([0-9a-fA-F]{64}) [ *](.+)$/.exec(line);
		if (!m) throw new PackVerificationError(`Malformed line in SHA256SUMS.txt: ${line}`);
		sums.set(m[2]!, m[1]!.toLowerCase());
	}
	return sums;
}

/** Fails on any missing file or any hash mismatch; returns the verified path → hash map. */
export function verifyPack(source: string): Map<string, string> {
	const sumsPath = join(source, 'SHA256SUMS.txt');
	if (!existsSync(sumsPath)) throw new PackVerificationError(`Missing ${sumsPath}`);
	const sums = parseChecksums(readFileSync(sumsPath, 'utf8'));
	if (sums.size === 0) throw new PackVerificationError('SHA256SUMS.txt lists no files');
	const problems: string[] = [];
	for (const [file, expected] of sums) {
		const path = join(source, file);
		if (!existsSync(path)) {
			problems.push(`missing: ${file}`);
		} else if (sha256(readFileSync(path)) !== expected) {
			problems.push(`mismatch: ${file}`);
		}
	}
	if (problems.length) {
		throw new PackVerificationError(
			`Brand source pack failed verification against SHA256SUMS.txt:\n  ${problems.join('\n  ')}`
		);
	}
	return sums;
}

// ---------------------------------------------------------------------------------------------
// Scenes

export interface SceneFile {
	file: string;
	bytes: number;
}

/** Throws with a per-file report when any AVIF at 540w is over the ceiling. */
export function assertSceneBudget(files: SceneFile[], limit = AVIF_540_LIMIT_BYTES): void {
	const over = files.filter((f) => f.bytes > limit);
	if (!over.length) return;
	const rows = [...files]
		.sort((a, b) => b.bytes - a.bytes)
		.map((f) => `  ${f.bytes > limit ? 'OVER' : 'ok  '} ${f.bytes} B  ${f.file}`);
	throw new Error(
		`${over.length} scene AVIF(s) at 540w exceed ${limit} B (§10.19):\n${rows.join('\n')}`
	);
}

export interface SceneEntry {
	id: string;
	mode: Mode;
	width: number;
	height: number;
	srcset: { avif: string; webp: string };
}

const srcsetFor = (base: string, ext: 'avif' | 'webp') =>
	(ext === 'avif' ? SCENE_WIDTHS : WEBP_SCENE_WIDTHS)
		.map((w) => `/brand/scenes/${base}-${w}.${ext} ${w}w`)
		.join(', ');

interface StateManifest {
	stateAssets: Record<string, Record<Mode, string>>;
}

/** One entry per state id and mode, aliases included (they point at the same files). */
export function sceneEntries(manifest: StateManifest): SceneEntry[] {
	const entries: SceneEntry[] = [];
	for (const [id, modes] of Object.entries(manifest.stateAssets)) {
		for (const mode of ['light', 'dark'] as const) {
			const base = /([^/]+)\.png$/.exec(modes[mode])?.[1];
			if (!base) throw new Error(`Bad scene path for ${id}/${mode}: ${modes[mode]}`);
			entries.push({
				id,
				mode,
				width: SCENE_SOURCE.width,
				height: SCENE_SOURCE.height,
				srcset: { avif: srcsetFor(base, 'avif'), webp: srcsetFor(base, 'webp') }
			});
		}
	}
	return entries;
}

type Output = { path: string; data: Buffer | string };

async function buildScenes(p: Paths, manifest: StateManifest): Promise<Output[]> {
	const sources = new Set<string>();
	for (const modes of Object.values(manifest.stateAssets)) {
		sources.add(modes.light);
		sources.add(modes.dark);
	}
	const outputs: Output[] = [];
	const budget: SceneFile[] = [];
	await Promise.all(
		[...sources].map(async (rel) => {
			const base = /([^/]+)\.png$/.exec(rel)![1]!;
			const input = readFileSync(join(p.source, rel));
			for (const width of SCENE_WIDTHS) {
				const resized = () => sharp(input).resize({ width });
				const avif = await resized().avif(SCENE_AVIF).toBuffer();
				outputs.push({ path: join(p.staticOut, 'scenes', `${base}-${width}.avif`), data: avif });
				if ((WEBP_SCENE_WIDTHS as readonly number[]).includes(width)) {
					const webp = await resized().webp(SCENE_WEBP).toBuffer();
					outputs.push({ path: join(p.staticOut, 'scenes', `${base}-${width}.webp`), data: webp });
				}
				if (width === 540) budget.push({ file: `${base}-540.avif`, bytes: avif.length });
			}
		})
	);
	assertSceneBudget(budget);
	return outputs;
}

// ---------------------------------------------------------------------------------------------
// Logos

export interface LogoEntry {
	id: LogoKind;
	mode: Mode;
	width: number;
	height: number;
	src: { avif: string; webp: string };
}

async function buildLogos(p: Paths): Promise<{ outputs: Output[]; entries: LogoEntry[] }> {
	const outputs: Output[] = [];
	const entries: LogoEntry[] = [];
	for (const kind of Object.keys(LOGO_DISPLAY_WIDTH) as LogoKind[]) {
		for (const mode of ['light', 'dark'] as const) {
			const input = readFileSync(join(p.source, 'logos', `${kind}_${mode}_transparent.png`));
			const trimmed = await sharp(input).trim().toBuffer();
			const meta = await sharp(trimmed).metadata();
			const width = LOGO_DISPLAY_WIDTH[kind];
			const height = Math.round((meta.height! * width) / meta.width!);
			for (const scale of [1, 2]) {
				const resized = () => sharp(trimmed).resize({ width: width * scale });
				const avif = await resized().avif({ quality: 40, effort: 4 }).toBuffer();
				const webp = await resized().webp({ quality: 50, alphaQuality: 80, effort: 6 }).toBuffer();
				const name = `${kind}_${mode}@${scale}x`;
				outputs.push({ path: join(p.staticOut, 'logos', `${name}.avif`), data: avif });
				outputs.push({ path: join(p.staticOut, 'logos', `${name}.webp`), data: webp });
			}
			const set = (ext: 'avif' | 'webp') =>
				`/brand/logos/${kind}_${mode}@1x.${ext} 1x, /brand/logos/${kind}_${mode}@2x.${ext} 2x`;
			entries.push({
				id: kind,
				mode,
				width,
				height,
				src: { avif: set('avif'), webp: set('webp') }
			});
		}
	}
	return { outputs, entries };
}

// ---------------------------------------------------------------------------------------------
// Compass SVG

export type CompassRole = 'surface' | 'primary' | 'accent' | 'star';
export type CompassPalette = Record<CompassRole, string>;

const ELEMENT = /<(circle|path)\b([^>]*?)\/>/g;
const COLOUR_ATTR = /\b(fill|stroke)="(#[0-9a-fA-F]{3,8})"/;

/** Role of a compass element: the shape tells which colour slot it uses. */
function roleOf(tag: string, attrs: string): CompassRole {
	if (tag === 'path') return /\bstroke="#/.test(attrs) ? 'primary' : 'star';
	const r = Number(/\br="([\d.]+)"/.exec(attrs)?.[1]);
	// The disc and the hub (r = 28) share the surface colour; the two small nodes are the accent.
	return r > 100 || r === 28 ? 'surface' : 'accent';
}

const minifySvg = (svg: string) =>
	svg
		.replace(/<\?xml[\s\S]*?\?>/g, '')
		.replace(/<!--[\s\S]*?-->/g, '')
		.replace(/>\s+</g, '><')
		.replace(/\s{2,}/g, ' ')
		.trim();

/** Colours by role, read from a compass SVG. */
export function compassPalette(svg: string): CompassPalette {
	const palette: Partial<CompassPalette> = {};
	for (const [, tag, attrs] of minifySvg(svg).matchAll(ELEMENT)) {
		const colour = COLOUR_ATTR.exec(attrs!)?.[2]?.toUpperCase();
		if (!colour) continue;
		const role = roleOf(tag!, attrs!);
		if (palette[role] && palette[role] !== colour) {
			throw new Error(`Compass role "${role}" has two colours: ${palette[role]} and ${colour}`);
		}
		palette[role] = colour;
	}
	for (const role of ['surface', 'primary', 'accent', 'star'] as const) {
		if (!palette[role]) throw new Error(`Compass SVG has no "${role}" element`);
	}
	return palette as CompassPalette;
}

/** The SVG with every colour attribute replaced by a role placeholder, for geometry comparison. */
function geometry(svg: string): string {
	return minifySvg(svg).replace(ELEMENT, (el, tag: string, attrs: string) =>
		el.replace(COLOUR_ATTR, (_m, attr: string) => `${attr}="${roleOf(tag, attrs)}"`)
	);
}

/**
 * Minify the light compass and rewrite its colours to `var(--brand-<role>, <light value>)`.
 * Refuses when the dark file differs in anything but colour, since one mark must serve both modes.
 */
export function buildCompass(lightSvg: string, darkSvg: string): string {
	if (geometry(lightSvg) !== geometry(darkSvg)) {
		throw new Error('compass_light.svg and compass_dark.svg differ in more than colour');
	}
	const palette = compassPalette(lightSvg);
	return minifySvg(lightSvg).replace(ELEMENT, (el, tag: string, attrs: string) =>
		el.replace(COLOUR_ATTR, (_m, attr: string) => {
			const role = roleOf(tag, attrs);
			return `${attr}="var(--brand-${role},${palette[role]})"`;
		})
	);
}

// ---------------------------------------------------------------------------------------------
// Favicons

/** SVG favicon: the light palette by default, the dark one under prefers-color-scheme. */
export function buildFaviconSvg(lightSvg: string, darkSvg: string): string {
	const light = compassPalette(lightSvg);
	const dark = compassPalette(darkSvg);
	const rules = (c: CompassPalette) =>
		`.s{fill:${c.surface}}.p{stroke:${c.primary}}.a{fill:${c.accent}}.g{fill:${c.star}}`;
	const style = `<style>${rules(light)}@media(prefers-color-scheme:dark){${rules(dark)}}</style>`;
	const cls: Record<CompassRole, string> = { surface: 's', primary: 'p', accent: 'a', star: 'g' };
	const body = minifySvg(lightSvg)
		.replace(/^<svg[^>]*>/, '')
		.replace(/<\/svg>$/, '')
		.replace(ELEMENT, (el, tag: string, attrs: string) =>
			el.replace(COLOUR_ATTR, () => {
				// The colour moves into the stylesheet; stroke width, caps and fill="none" stay attributes.
				return `class="${cls[roleOf(tag, attrs)]}"`;
			})
		);
	return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512">${style}${body}</svg>`;
}

/** Point the manifest's icon paths at the served /brand/favicons/ location. */
export function fixManifest(json: string): string {
	const manifest = JSON.parse(json) as { icons?: { src: string }[] };
	for (const icon of manifest.icons ?? []) {
		icon.src = `/brand/favicons/${icon.src.split('/').pop()}`;
	}
	return `${JSON.stringify(manifest, null, '\t')}\n`;
}

// ---------------------------------------------------------------------------------------------
// Windows .ico

/** ICO container holding PNG-compressed images (Vista and later). */
export function encodeIco(images: { size: number; png: Buffer }[]): Buffer {
	const header = Buffer.alloc(6 + 16 * images.length);
	header.writeUInt16LE(0, 0);
	header.writeUInt16LE(1, 2);
	header.writeUInt16LE(images.length, 4);
	let offset = header.length;
	images.forEach(({ size, png }, i) => {
		const at = 6 + 16 * i;
		header.writeUInt8(size >= 256 ? 0 : size, at);
		header.writeUInt8(size >= 256 ? 0 : size, at + 1);
		header.writeUInt16LE(1, at + 4); // colour planes
		header.writeUInt16LE(32, at + 6); // bits per pixel
		header.writeUInt32LE(png.length, at + 8);
		header.writeUInt32LE(offset, at + 12);
		offset += png.length;
	});
	return Buffer.concat([header, ...images.map((i) => i.png)]);
}

async function buildPackagingIcons(p: Paths): Promise<Output[]> {
	const src = readFileSync(join(p.source, 'icons', 'mascot_app_dark_256.png'));
	const images = await Promise.all(
		ICO_SIZES.map(async (size) => ({
			size,
			png: await sharp(src).resize(size, size).png({ compressionLevel: 9 }).toBuffer()
		}))
	);
	return [
		{ path: join(p.packagingOut, 'lodestar.ico'), data: encodeIco(images) },
		{ path: join(p.packagingOut, 'lodestar-256.png'), data: src }
	];
}

// ---------------------------------------------------------------------------------------------
// Orchestration

async function formatTs(path: string, source: string): Promise<string> {
	const config = (await resolveConfig(path)) ?? {};
	return format(source, { ...config, filepath: path });
}

async function generatedModule(
	path: string,
	scenes: SceneEntry[],
	logos: LogoEntry[]
): Promise<string> {
	const source = `// Generated by scripts/assets.ts from brand/source/. Do not edit.
export type SceneMode = 'light' | 'dark';

export interface Scene {
	id: string;
	mode: SceneMode;
	/** Intrinsic size of the largest variant; set as width/height attributes so nothing shifts. */
	width: number;
	height: number;
	srcset: { avif: string; webp: string };
}

export interface Logo {
	id: 'wordmark' | 'horizontal' | 'stacked';
	mode: SceneMode;
	/** Display size in CSS px (the 1x variant). */
	width: number;
	height: number;
	/** 1x/2x srcset strings. */
	src: { avif: string; webp: string };
}

export const scenes: readonly Scene[] = ${JSON.stringify(scenes)};

export const logos: readonly Logo[] = ${JSON.stringify(logos)};
`;
	return formatTs(path, source);
}

export interface Report {
	skipped: boolean;
	totalBytes: number;
}

function directorySize(dir: string, outputs: Output[]): number {
	return outputs
		.filter((o) => o.path.startsWith(dir))
		.reduce(
			(n, o) => n + (typeof o.data === 'string' ? Buffer.byteLength(o.data) : o.data.length),
			0
		);
}

export async function runAssets(p: Paths = defaultPaths()): Promise<Report> {
	const sums = verifyPack(p.source);

	const stamp = sha256(
		JSON.stringify({
			sums: [...sums],
			script: sha256(readFileSync(fileURLToPath(import.meta.url))),
			sharp: sharp.versions.sharp
		})
	);
	const stampPath = join(p.cacheDir, 'stamp.json');
	const readStamp = (): { stamp?: string; files?: string[] } => {
		try {
			return JSON.parse(readFileSync(stampPath, 'utf8'));
		} catch {
			return {};
		}
	};
	const previous = readStamp();
	if (previous.stamp === stamp && previous.files?.every((f) => existsSync(f))) {
		return { skipped: true, totalBytes: 0 };
	}

	const manifest = JSON.parse(
		readFileSync(join(p.source, 'visuals', 'manifest.json'), 'utf8')
	) as StateManifest;
	const read = (rel: string) => readFileSync(join(p.source, rel), 'utf8');

	const scenes = await buildScenes(p, manifest);
	const logos = await buildLogos(p);

	const lightSvg = read('icons/compass_light.svg');
	const darkSvg = read('icons/compass_dark.svg');
	const outputs: Output[] = [...scenes, ...logos.outputs];

	const favicons = join(p.staticOut, 'favicons');
	// The outputs are build products, not sources: keep them out of git (the folder ignores itself).
	outputs.push(
		{ path: join(p.staticOut, '.gitignore'), data: '*\n' },
		{ path: join(favicons, 'favicon.svg'), data: buildFaviconSvg(lightSvg, darkSvg) },
		{
			path: join(favicons, 'site.webmanifest'),
			data: fixManifest(read('favicons/site.webmanifest'))
		}
	);
	for (const name of [
		'favicon.ico',
		'apple-touch-icon.png',
		'android-chrome-192x192.png',
		'android-chrome-512x512.png'
	]) {
		outputs.push({
			path: join(favicons, name),
			data: readFileSync(join(p.source, 'favicons', name))
		});
	}
	outputs.push(...(await buildPackagingIcons(p)));

	const compassPath = join(p.libOut, 'compass.svg');
	const modulePath = join(p.libOut, 'scenes.generated.ts');
	outputs.push(
		{ path: compassPath, data: buildCompass(lightSvg, darkSvg) },
		{
			path: modulePath,
			data: await generatedModule(modulePath, sceneEntries(manifest), logos.entries)
		}
	);

	// Everything validated: replace the previous outputs.
	rmSync(p.staticOut, { recursive: true, force: true });
	for (const o of outputs) {
		mkdirSync(dirname(o.path), { recursive: true });
		writeFileSync(o.path, o.data);
	}
	mkdirSync(p.cacheDir, { recursive: true });
	writeFileSync(stampPath, JSON.stringify({ stamp, files: outputs.map((o) => o.path) }));

	return { skipped: false, totalBytes: directorySize(p.staticOut, outputs) };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
	runAssets().then(
		(r) =>
			console.log(
				r.skipped
					? 'assets: up to date'
					: `assets: wrote ${(r.totalBytes / 1024).toFixed(0)} KiB under static/brand/`
			),
		(err: unknown) => {
			console.error(err instanceof Error ? err.message : err);
			process.exit(1);
		}
	);
}
