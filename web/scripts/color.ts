// Colour maths for the theme compiler (architecture §10.18). In-house, no dependencies:
// sRGB <-> OKLab/OKLCH, WCAG 2.x contrast, ΔE_ok and colour-vision-deficiency simulation.

export type Rgb = [number, number, number]; // sRGB, 0..1, gamma-encoded
export type Lch = { l: number; c: number; h: number }; // OKLCH: L 0..1, C >= 0, h in degrees

const clamp01 = (x: number) => Math.min(1, Math.max(0, x));

// ---- hex <-> sRGB -----------------------------------------------------------------------

export function parseHex(hex: string): Rgb {
	const m = /^#([0-9a-f]{6})$/i.exec(hex.trim());
	if (!m) throw new Error(`invalid colour "${hex}" (expected #RRGGBB)`);
	const n = parseInt(m[1]!, 16);
	return [(n >> 16) / 255, ((n >> 8) & 255) / 255, (n & 255) / 255];
}

export function toHex(rgb: Rgb): string {
	return (
		'#' +
		rgb
			.map((v) =>
				Math.round(clamp01(v) * 255)
					.toString(16)
					.padStart(2, '0')
			)
			.join('')
			.toUpperCase()
	);
}

// ---- transfer function ------------------------------------------------------------------

export const toLinear = (v: number) =>
	v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
export const fromLinear = (v: number) =>
	v <= 0.0031308 ? v * 12.92 : 1.055 * Math.pow(v, 1 / 2.4) - 0.055;

export const linearRgb = (hex: string): Rgb => parseHex(hex).map(toLinear) as Rgb;
const hexFromLinear = (lin: Rgb) => toHex(lin.map((v) => fromLinear(clamp01(v))) as Rgb);

// ---- OKLab / OKLCH (Ottosson) -----------------------------------------------------------

export type Lab = [number, number, number];

function linearToOklab([r, g, b]: Rgb): Lab {
	const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
	const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
	const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
	return [
		0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
		1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
		0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s
	];
}

function oklabToLinear([L, a, b]: Lab): Rgb {
	const l = (L + 0.3963377774 * a + 0.2158037573 * b) ** 3;
	const m = (L - 0.1055613458 * a - 0.0638541728 * b) ** 3;
	const s = (L - 0.0894841775 * a - 1.291485548 * b) ** 3;
	return [
		4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
		-1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
		-0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s
	];
}

export const toOklab = (hex: string): Lab => linearToOklab(linearRgb(hex));
export const fromOklab = (lab: Lab): string => hexFromLinear(oklabToLinear(lab));

export function toOklch(hex: string): Lch {
	const [l, a, b] = toOklab(hex);
	const c = Math.hypot(a, b);
	// Achromatic colours have no meaningful hue; 0 keeps the maths deterministic.
	const h = c < 1e-4 ? 0 : ((Math.atan2(b, a) * 180) / Math.PI + 360) % 360;
	return { l, c, h };
}

const inGamut = (lin: Rgb) => lin.every((v) => v >= -1e-6 && v <= 1 + 1e-6);
const labOf = ({ l, c, h }: Lch): Lab => [
	l,
	c * Math.cos((h * Math.PI) / 180),
	c * Math.sin((h * Math.PI) / 180)
];

/** OKLCH to hex. Out-of-gamut colours keep L and h and lose only as much chroma as needed. */
export function fromOklch(lch: Lch): string {
	const lab = labOf(lch);
	if (inGamut(oklabToLinear(lab))) return fromOklab(lab);
	let lo = 0;
	let hi = lch.c;
	for (let i = 0; i < 30; i++) {
		const mid = (lo + hi) / 2;
		if (inGamut(oklabToLinear(labOf({ ...lch, c: mid })))) lo = mid;
		else hi = mid;
	}
	return fromOklab(labOf({ ...lch, c: lo }));
}

/** Shift lightness only: hue and chroma are kept (gamut permitting). */
export const withLightness = (hex: string, l: number): string =>
	fromOklch({ ...toOklch(hex), l: Math.min(1, Math.max(0, l)) });

/** Rotate hue and scale chroma, keeping lightness (the error hue shift). */
export const withHue = (hex: string, hueDelta: number, chromaScale = 1): string => {
	const { l, c, h } = toOklch(hex);
	return fromOklch({ l, c: c * chromaScale, h: (h + hueDelta + 360) % 360 });
};

/** Linear interpolation in OKLab; t = 0 gives `a`, t = 1 gives `b`. */
export function mix(a: string, b: string, t: number): string {
	const x = toOklab(a);
	const y = toOklab(b);
	return fromOklab([x[0] + (y[0] - x[0]) * t, x[1] + (y[1] - x[1]) * t, x[2] + (y[2] - x[2]) * t]);
}

// ---- WCAG 2.x contrast ------------------------------------------------------------------

export function luminance(hex: string): number {
	const [r, g, b] = linearRgb(hex);
	return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

export function contrast(a: string, b: string): number {
	const la = luminance(a);
	const lb = luminance(b);
	return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
}

/** The lighter-or-darker of `candidates` with the highest contrast against `on`. */
export const bestContrast = (on: string, candidates: string[]): string =>
	candidates.reduce((best, c) => (contrast(c, on) > contrast(best, on) ? c : best));

// ---- ΔE_ok ------------------------------------------------------------------------------

/** Euclidean distance in OKLab, scaled by 100 (so 1 is about one just-noticeable step). */
export function deltaE(a: string, b: string): number {
	const x = toOklab(a);
	const y = toOklab(b);
	return 100 * Math.hypot(x[0] - y[0], x[1] - y[1], x[2] - y[2]);
}

// ---- colour-vision-deficiency simulation (linear-RGB matrices, row-major) ----------------

export type Cvd = 'deuteranopia' | 'protanopia';
export type CvdModel = 'machado' | 'vienot';
type Mat = readonly [number, number, number, number, number, number, number, number, number];

const MATRICES: Record<CvdModel, Record<Cvd, Mat>> = {
	// Machado, Oliveira & Fernandes 2009, severity 1.0.
	machado: {
		deuteranopia: [
			0.367322, 0.860646, -0.227968, 0.280085, 0.672501, 0.047413, -0.01182, 0.04294, 0.968881
		],
		protanopia: [
			0.152286, 1.052583, -0.204868, 0.114503, 0.786281, 0.099216, -0.003882, -0.048116, 1.051998
		]
	},
	// Viénot, Brettel & Mollon 1999.
	vienot: {
		deuteranopia: [0.29275, 0.70725, 0, 0.29275, 0.70725, 0, -0.02234, 0.02234, 1],
		protanopia: [0.11238, 0.88762, 0, 0.11238, 0.88762, 0, 0.00401, -0.00401, 1]
	}
};

export function simulate(hex: string, kind: Cvd, model: CvdModel): string {
	const m = MATRICES[model][kind];
	const [r, g, b] = linearRgb(hex);
	return hexFromLinear([
		m[0] * r + m[1] * g + m[2] * b,
		m[3] * r + m[4] * g + m[5] * b,
		m[6] * r + m[7] * g + m[8] * b
	]);
}

/** Smallest pairwise ΔE_ok in a set, with the pair that produced it. */
export function minPairwise(
	colours: string[],
	transform: (hex: string) => string = (h) => h
): { min: number; pair: [number, number] } {
	let min = Infinity;
	let pair: [number, number] = [0, 0];
	for (let i = 0; i < colours.length; i++) {
		for (let j = i + 1; j < colours.length; j++) {
			const d = deltaE(transform(colours[i]!), transform(colours[j]!));
			if (d < min) {
				min = d;
				pair = [i, j];
			}
		}
	}
	return { min, pair };
}

// ---- searching for a lightness that meets a contrast target -----------------------------

/**
 * Move `hex` along OKLCH lightness (hue and chroma kept) until `ok(candidate)` first holds,
 * walking away from its current lightness in `direction`. Returns the candidate closest to
 * the original that satisfies `ok`, or `hex` itself when it already does.
 */
export function shiftUntil(
	hex: string,
	direction: 'darker' | 'lighter',
	ok: (candidate: string) => boolean
): string {
	if (ok(hex)) return hex;
	const { l } = toOklch(hex);
	const end = direction === 'darker' ? 0 : 1;
	if (!ok(withLightness(hex, end))) return withLightness(hex, end);
	// Bisect between "fails" (at l) and "passes" (at the end), then step on the hex grid so the
	// result is the nearest 8-bit colour that passes.
	let fail = l;
	let pass = end;
	for (let i = 0; i < 40; i++) {
		const mid = (fail + pass) / 2;
		if (ok(withLightness(hex, mid))) pass = mid;
		else fail = mid;
	}
	return withLightness(hex, pass);
}
