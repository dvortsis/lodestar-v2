import { describe, expect, it } from 'vitest';
import {
	contrast,
	deltaE,
	fromOklch,
	minPairwise,
	mix,
	parseHex,
	shiftUntil,
	simulate,
	toHex,
	toOklab,
	toOklch,
	withLightness
} from './color.ts';

describe('hex and sRGB', () => {
	it('round-trips', () => {
		expect(toHex(parseHex('#243c44'))).toBe('#243C44');
		expect(toHex(parseHex('#FFFFFF'))).toBe('#FFFFFF');
	});
	it('rejects malformed colours', () => {
		for (const bad of ['fff', '#fff', '#12345', '#GGGGGG', '']) {
			expect(() => parseHex(bad)).toThrow(/invalid colour/);
		}
	});
});

describe('OKLab / OKLCH', () => {
	it('maps white and black to the L axis', () => {
		const [l, a, b] = toOklab('#FFFFFF');
		expect(l).toBeCloseTo(1, 4);
		expect(a).toBeCloseTo(0, 4);
		expect(b).toBeCloseTo(0, 4);
		expect(toOklab('#000000')[0]).toBeCloseTo(0, 6);
	});
	it('matches published reference values', () => {
		// Ottosson's reference: sRGB red is OKLab (0.62796, 0.22486, 0.12585).
		const [l, a, b] = toOklab('#FF0000');
		expect(l).toBeCloseTo(0.62796, 4);
		expect(a).toBeCloseTo(0.22486, 4);
		expect(b).toBeCloseTo(0.12585, 4);
	});
	it('round-trips every sampled colour through OKLCH', () => {
		let seed = 12345;
		const next = () => (seed = (seed * 1664525 + 1013904223) >>> 0);
		for (let i = 0; i < 500; i++) {
			const hex = '#' + (next() >>> 8).toString(16).padStart(6, '0').toUpperCase();
			expect(fromOklch(toOklch(hex))).toBe(hex);
		}
	});
	it('keeps hue and chroma when only lightness moves', () => {
		for (const hex of ['#527F91', '#829F8A', '#C4A765', '#BF8173']) {
			const before = toOklch(hex);
			const after = toOklch(withLightness(hex, before.l - 0.1));
			expect(after.l).toBeCloseTo(before.l - 0.1, 2);
			expect(Math.abs(after.h - before.h)).toBeLessThan(1.5);
			expect(Math.abs(after.c - before.c)).toBeLessThan(0.004);
		}
	});
	it('stays in gamut by giving up chroma, not lightness or hue', () => {
		const out = withLightness('#FF0000', 0.95);
		expect(out).toMatch(/^#[0-9A-F]{6}$/);
		expect(toOklch(out).l).toBeCloseTo(0.95, 2);
	});
	it('mixes in OKLab', () => {
		expect(mix('#000000', '#FFFFFF', 0)).toBe('#000000');
		expect(mix('#000000', '#FFFFFF', 1)).toBe('#FFFFFF');
	});
});

describe('WCAG contrast', () => {
	it('is 21:1 for black on white and 1:1 for identical colours', () => {
		expect(contrast('#000000', '#FFFFFF')).toBeCloseTo(21, 6);
		expect(contrast('#527F91', '#527F91')).toBeCloseTo(1, 6);
	});
	it('is symmetric', () => {
		expect(contrast('#243C44', '#F3F6F5')).toBe(contrast('#F3F6F5', '#243C44'));
	});
	it('reproduces the audit figures in architecture §10.18', () => {
		expect(contrast('#243C44', '#F3F6F5')).toBeCloseTo(10.7, 1);
		expect(contrast('#243C44', '#FFFFFF')).toBeCloseTo(11.6, 1);
		expect(contrast('#527F91', '#F3F6F5')).toBeCloseTo(4.02, 2);
		expect(contrast('#527F91', '#FFFFFF')).toBeCloseTo(4.37, 2);
		expect(contrast('#829F8A', '#F3F6F5')).toBeCloseTo(2.65, 2);
		expect(contrast('#E5EEED', '#141F25')).toBeCloseTo(14.2, 1);
	});
});

describe('ΔE_ok', () => {
	it('is zero for equal colours and symmetric', () => {
		expect(deltaE('#527F91', '#527F91')).toBe(0);
		expect(deltaE('#527F91', '#BF8173')).toBeCloseTo(deltaE('#BF8173', '#527F91'), 10);
	});
	it('is 100 between black and white', () => {
		expect(deltaE('#000000', '#FFFFFF')).toBeCloseTo(100, 3);
	});
	it('reproduces the weakest Riverstone pair in §10.18 (sage vs slate, ≈ 5.1)', () => {
		expect(deltaE('#829F8A', '#8C9BA7')).toBeCloseTo(5.1, 0);
	});
});

describe('colour-vision simulation', () => {
	const models = ['machado', 'vienot'] as const;
	const kinds = ['deuteranopia', 'protanopia'] as const;

	it('leaves greys alone (matrix rows sum to 1)', () => {
		for (const model of models) {
			for (const kind of kinds) {
				for (const grey of ['#000000', '#808080', '#FFFFFF']) {
					const out = parseHex(simulate(grey, kind, model));
					const ref = parseHex(grey);
					out.forEach((v, i) => expect(Math.abs(v - ref[i]!)).toBeLessThan(2 / 255));
				}
			}
		}
	});
	it('collapses red and green under deuteranopia', () => {
		for (const model of models) {
			const d = deltaE(
				simulate('#FF0000', 'deuteranopia', model),
				simulate('#00B000', 'deuteranopia', model)
			);
			expect(d).toBeLessThan(deltaE('#FF0000', '#00B000') / 2);
		}
	});
	it('differs between the two models without disagreeing wildly', () => {
		const a = simulate('#9381B6', 'deuteranopia', 'machado');
		const b = simulate('#9381B6', 'deuteranopia', 'vienot');
		expect(a).not.toBe(b);
		expect(deltaE(a, b)).toBeLessThan(8);
	});
	it('finds the closest pair in a set', () => {
		const r = minPairwise(['#808080', '#FFFFFF', '#818181']);
		expect(r.pair).toEqual([0, 2]);
		expect(r.min).toBeLessThan(1);
	});
});

describe('shiftUntil', () => {
	it('returns the colour unchanged when it already passes', () => {
		expect(shiftUntil('#243C44', 'darker', (c) => contrast(c, '#FFFFFF') >= 4.5)).toBe('#243C44');
	});
	it('stops at the nearest passing lightness', () => {
		const out = shiftUntil('#527F91', 'darker', (c) => contrast(c, '#F3F6F5') >= 4.5);
		expect(contrast(out, '#F3F6F5')).toBeGreaterThanOrEqual(4.5);
		expect(contrast(out, '#F3F6F5')).toBeLessThan(4.55);
	});
	it('gives up at the extreme when nothing passes', () => {
		expect(shiftUntil('#527F91', 'darker', () => false)).toBe('#000000');
	});
});
