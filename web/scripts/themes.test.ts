// @vitest-environment node
import { spawnSync } from 'node:child_process';
import { cpSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { afterAll, describe, expect, it } from 'vitest';
import { contrast, deltaE, toOklch } from './color.ts';
import {
	DATA_SLOT,
	PREPAINT,
	TOKEN_KEYS,
	compileDir,
	compileFamilies,
	labelOn,
	derive,
	evaluate,
	failures,
	injectPrepaint,
	renderOutputs,
	PINS,
	TINT_GAP,
	expandFamily,
	validateFamily,
	type Family,
	type Theme,
	type Tokens
} from './themes.ts';

const root = fileURLToPath(new URL('..', import.meta.url));
const themesDir = join(root, 'themes');
const riverstone = JSON.parse(readFileSync(join(themesDir, 'riverstone.json'), 'utf8')) as Family;
const [light, dark] = expandFamily(riverstone) as [Theme, Theme];
const L = derive(light);
const D = derive(dark);

// Every token design-tokens.md §1-4 defines for Riverstone, light and dark, byte for byte (36 each).
// Values the doc leaves to a rule (the *-content labels and neutral-content, which must reach
// 4.5:1 on their fill) are the compiler's choice, pinned here so a rule change cannot slip by.
const CONTRACT: Record<'light' | 'dark', Tokens> = {
	light: {
		'--color-base-100': '#FFFFFF',
		'--color-base-200': '#F3F6F5',
		'--color-base-300': '#DCE4E1',
		'--color-base-content': '#243C44',
		'--color-primary': '#4A7789',
		'--color-primary-content': '#FFFFFF',
		'--color-secondary': '#4A7789',
		'--color-secondary-content': '#FFFFFF',
		'--color-accent': '#829F8A',
		'--color-accent-content': '#243C44',
		'--color-neutral': '#8796A2',
		'--color-neutral-content': '#193038',
		'--color-info': '#4A7789',
		'--color-info-content': '#FFFFFF',
		'--color-success': '#5B7763',
		'--color-success-content': '#FFFFFF',
		'--color-warning': '#896D2A',
		'--color-warning-content': '#FFFFFF',
		'--color-error': '#B45253',
		'--color-error-content': '#FFFFFF',
		'--color-accent-ink': '#5B7763',
		'--color-muted-text': '#567079',
		'--color-field-border': '#8796A2',
		'--color-hover': '#F1F5F6',
		'--color-selected': '#E7EDF0',
		'--data-1': '#527F91',
		'--data-2': '#AE9150',
		'--data-3': '#BF8173',
		'--data-4': '#8796A2',
		'--data-5': '#7E9B86',
		'--data-6': '#9381B6',
		'--data-other': '#567079',
		'--brand-surface': '#FFFFFF',
		'--brand-primary': '#527F91',
		'--brand-accent': '#829F8A',
		'--brand-star': '#D49A4A'
	},
	dark: {
		'--color-base-100': '#22313A',
		'--color-base-200': '#141F25',
		'--color-base-300': '#354851',
		'--color-base-content': '#E5EEED',
		'--color-primary': '#85AFC0',
		'--color-primary-content': '#141F25',
		'--color-secondary': '#85AFC0',
		'--color-secondary-content': '#141F25',
		'--color-accent': '#A0C3A7',
		'--color-accent-content': '#141F25',
		'--color-neutral': '#8C9BA7',
		'--color-neutral-content': '#22313A',
		'--color-info': '#85AFC0',
		'--color-info-content': '#141F25',
		'--color-success': '#A0C3A7',
		'--color-success-content': '#141F25',
		'--color-warning': '#C4A765',
		'--color-warning-content': '#141F25',
		'--color-error': '#DC8080',
		'--color-error-content': '#141F25',
		'--color-accent-ink': '#A0C3A7',
		'--color-muted-text': '#B2C3C5',
		'--color-field-border': '#8C9BA7',
		'--color-hover': '#2A3B45',
		'--color-selected': '#30434D',
		'--data-1': '#527F91',
		'--data-2': '#C4A765',
		'--data-3': '#BF8173',
		'--data-4': '#8C9BA7',
		'--data-5': '#829F8A',
		'--data-6': '#9381B6',
		'--data-other': '#B2C3C5',
		'--brand-surface': '#22313A',
		'--brand-primary': '#85AFC0',
		'--brand-accent': '#A0C3A7',
		'--brand-star': '#E0A95F'
	}
};

describe('Riverstone golden tokens (architecture §10.18)', () => {
	for (const [mode, tokens] of [
		['light', L],
		['dark', D]
	] as const) {
		it(`${mode}: all 36 contract tokens, byte for byte`, () => {
			expect(tokens).toEqual(CONTRACT[mode]);
			expect(Object.keys(CONTRACT[mode])).toEqual([...TOKEN_KEYS]);
		});
		it(`${mode}: the rules alone land within ΔE_ok ≤ 0.5 of every pinned value`, () => {
			const theme = mode === 'light' ? light : dark;
			const raw = derive({ ...theme, id: `probe-${mode}` }); // no pins
			for (const [key, want] of Object.entries(PINS[theme.id]!)) {
				const got = raw[key as keyof Tokens];
				expect(deltaE(got, want), `${mode} ${key}: ${got} vs ${want}`).toBeLessThanOrEqual(0.5);
			}
		});
	}

	it('light data marks by slot are blue, ochre, terracotta, slate, sage, plum', () => {
		const want = ['#527F91', '#AE9150', '#BF8173', '#8796A2', '#7E9B86', '#9381B6'];
		[1, 2, 3, 4, 5, 6].forEach((slot, i) => {
			expect(L[`--data-${slot}` as keyof Tokens]).toBe(want[i]);
		});
	});

	it('neutral-content reaches 4.5:1 on neutral by a lightness-only shift', () => {
		for (const t of [L, D]) {
			expect(contrast(t['--color-neutral-content'], t['--color-neutral'])).toBeGreaterThanOrEqual(
				4.5
			);
		}
	});

	it('maps categorical[i] to slot [1,5,2,3,4,6][i]', () => {
		expect(DATA_SLOT).toEqual([1, 5, 2, 3, 4, 6]);
		const swapped: Theme = {
			...dark,
			colors: {
				...dark.colors,
				categorical: ['#FF8080', '#80FF80', '#8080FF', '#FFFF80', '#80FFFF', '#FF80FF']
			}
		};
		const t = derive(swapped);
		// dark theme: colours that already pass are untouched, so the slot is visible.
		expect(t['--data-1']).toBe('#FF8080');
		expect(t['--data-5']).toBe('#80FF80');
		expect(t['--data-2']).toBe('#8080FF');
		expect(t['--data-3']).toBe('#FFFF80');
		expect(t['--data-4']).toBe('#80FFFF');
		expect(t['--data-6']).toBe('#FF80FF');
	});

	it('emits every token in design-tokens.md §1-4, in order', () => {
		expect(Object.keys(L)).toEqual([...TOKEN_KEYS]);
		expect(TOKEN_KEYS).toContain('--data-other');
		expect(TOKEN_KEYS.filter((k) => k.startsWith('--brand-'))).toHaveLength(4);
	});

	it('meets the contrast documented for the derived tokens', () => {
		expect(contrast(L['--color-primary'], '#F3F6F5')).toBeGreaterThanOrEqual(4.5);
		expect(contrast('#FFFFFF', L['--color-primary'])).toBeCloseTo(4.89, 2);
		expect(contrast(L['--color-muted-text'], '#F3F6F5')).toBeCloseTo(4.84, 1);
		expect(contrast(D['--color-muted-text'], '#141F25')).toBeCloseTo(9.19, 0);
		expect(contrast(D['--color-error'], '#22313A')).toBeCloseTo(4.75, 1);
		expect(contrast(D['--color-warning'], '#22313A')).toBeCloseTo(5.78, 1);
	});

	it('passes every gate with no failures', () => {
		for (const [theme, tokens] of [
			[light, L],
			[dark, D]
		] as const) {
			const report = evaluate(theme, tokens);
			expect(failures(report).map((c) => c.label)).toEqual([]);
			expect(report.cvd.map((r) => r.kind)).toEqual(['deuteranopia', 'protanopia']);
			for (const row of report.cvd) {
				expect(row.machado).toBeGreaterThan(0);
				expect(row.vienot).toBeGreaterThan(0);
			}
		}
	});
});

describe('derivation rules', () => {
	const base: Theme = {
		...light,
		id: 'probe-light',
		colors: { ...light.colors, categorical: [...light.colors.categorical] }
	};

	it('keeps hue and chroma of the inks (lightness-only shift)', () => {
		const t = derive(base);
		for (const [ink, source] of [
			[t['--color-primary'], light.colors.primary],
			[t['--color-accent-ink'], light.colors.accent],
			[t['--color-warning'], light.colors.categorical[2]!],
			[t['--data-2'], light.colors.categorical[2]!]
		] as const) {
			const a = toOklch(ink);
			const b = toOklch(source);
			expect(a.l).toBeLessThan(b.l);
			expect(Math.abs(a.h - b.h), `${ink} hue`).toBeLessThan(1.5);
			expect(Math.abs(a.c - b.c), `${ink} chroma`).toBeLessThan(0.005);
		}
	});

	it('leaves a colour that already passes untouched', () => {
		const t = derive({ ...base, colors: { ...base.colors, primary: '#1B3A4B' } });
		expect(t['--color-primary']).toBe('#1B3A4B');
	});

	it('lightens inks on dark themes and darkens them on light themes', () => {
		const weakDark = derive({
			...dark,
			colors: { ...dark.colors, primary: '#2A4A5A', accent: '#3A5A4A' }
		});
		expect(contrast(weakDark['--color-primary'], '#141F25')).toBeGreaterThanOrEqual(4.5);
		expect(contrast(weakDark['--color-accent-ink'], '#22313A')).toBeGreaterThanOrEqual(4.5);
	});

	it('chooses content colours by contrast', () => {
		expect(L['--color-primary-content']).toBe('#FFFFFF');
		expect(D['--color-primary-content']).toBe('#141F25');
	});
});

describe('gates', () => {
	it('fails text below 4.5:1', () => {
		const bad: Theme = { ...light, id: 'bad', colors: { ...light.colors, text: '#8C9BA7' } };
		const report = evaluate(bad, derive(bad));
		const labels = failures(report).map((c) => c.label);
		expect(labels).toContain('text on background');
		expect(labels).toContain('text on surface');
	});

	it('requires 7:1 text for the High Contrast family', () => {
		const hc: Theme = {
			...light,
			id: 'high-contrast-light',
			family: 'High Contrast',
			colors: { ...light.colors, text: '#4A5F67' }
		};
		const normal = evaluate({ ...hc, family: 'Other' }, derive({ ...hc, family: 'Other' }));
		const strict = evaluate(hc, derive(hc));
		expect(failures(normal).map((c) => c.label)).not.toContain('text on background');
		expect(failures(strict).map((c) => c.label)).toContain('text on background');
	});

	it('warns, not fails, on close categorical colours, and reports the CVD simulation', () => {
		const close: Theme = {
			...dark,
			id: 'close-dark',
			family: 'Other',
			colors: {
				...dark.colors,
				categorical: ['#527F91', '#5A8799', '#C4A765', '#BF8173', '#8C9BA7', '#9381B6']
			}
		};
		const report = evaluate(close, derive(close));
		expect(report.normalDeltaE).toBeLessThan(5);
		expect(failures(report)).toEqual([]);
		expect(report.checks.some((c) => !c.pass && c.severity === 'warn')).toBe(true);
		expect(report.cvd[0]!.machado).not.toBe(report.cvd[0]!.vienot);
	});

	it('data colours reach 3:1 on the surface however pale the input', () => {
		const pale: Theme = {
			...light,
			id: 'pale-light',
			colors: {
				...light.colors,
				categorical: ['#DDEEFF', '#EEDDFF', '#FFEEDD', '#DDFFEE', '#EEFFDD', '#FFDDEE']
			}
		};
		const t = derive(pale);
		for (let n = 1; n <= 6; n++) {
			expect(contrast(t[`--data-${n}` as keyof Tokens], '#FFFFFF')).toBeGreaterThanOrEqual(3);
		}
	});
});

describe('schema validation', () => {
	it('accepts Riverstone', () => {
		expect(validateFamily(riverstone)).toEqual([]);
	});
	it('names every problem', () => {
		const errors = validateFamily({
			id: 'Bad Id',
			name: '',
			group: 'y',
			light: {
				background: 'red',
				surface: '#FFF',
				text: '#000000',
				primary: '#000000',
				accent: '#000000',
				categorical: ['#000000']
			}
		}).join('\n');
		expect(errors).toMatch(/"id"/);
		expect(errors).toMatch(/"name"/);
		expect(errors).toMatch(/"group"/);
		expect(errors).toMatch(/light\.background/);
		expect(errors).toMatch(/light\.surface/);
		expect(errors).toMatch(/light\.categorical must hold exactly 6/);
		expect(errors).toMatch(/"dark" must be an object/);
	});
	it('rejects non-objects', () => {
		expect(validateFamily(null)).toEqual(['not a JSON object']);
		expect(validateFamily([])).toEqual(['not a JSON object']);
	});
	it('expands a family into <id>-light and <id>-dark', () => {
		expect(expandFamily(riverstone).map((t) => [t.id, t.mode, t.family, t.group])).toEqual([
			['riverstone-light', 'light', 'Riverstone', 'official'],
			['riverstone-dark', 'dark', 'Riverstone', 'official']
		]);
	});
});

describe('npm run themes (CLI)', () => {
	const tmp = mkdtempSync(join(tmpdir(), 'themes-'));
	afterAll(() => rmSync(tmp, { recursive: true, force: true }));

	const run = (dir: string) =>
		spawnSync(
			process.execPath,
			['--experimental-strip-types', join(root, 'scripts/themes.ts'), '--dir', dir, '--no-write'],
			{ encoding: 'utf8' }
		);
	const copy = (name: string) => {
		const dir = join(tmp, name);
		cpSync(themesDir, dir, { recursive: true });
		return dir;
	};

	it('exits 0 for Riverstone', () => {
		const r = run(themesDir);
		expect(r.status, r.stderr).toBe(0);
		expect(r.stdout).toContain('48 theme(s) pass');
	});

	it('exits 1, with a readable report, for a fixture whose text contrast is below 4.5:1', () => {
		const dir = copy('failing');
		const bad: Family = {
			...riverstone,
			id: 'fixture-low-contrast',
			name: 'Fixture',
			light: { ...riverstone.light, text: '#8C9BA7' }
		};
		writeFileSync(join(dir, 'fixture-low-contrast.json'), JSON.stringify(bad));
		const r = run(dir);
		expect(r.status).toBe(1);
		expect(r.stderr).toContain('✗ fixture-low-contrast-light');
		expect(r.stderr).toMatch(/FAIL text on background: \d\.\d\d \(needs ≥ 4\.5\)/);
		expect(r.stderr).toContain('nothing written');
	});

	it('exits 1 on invalid JSON and when Riverstone is missing', () => {
		const dir = copy('broken');
		writeFileSync(join(dir, 'riverstone.json'), '{ not json');
		const r = run(dir);
		expect(r.status).toBe(1);
		expect(r.stderr).toContain('invalid JSON');
		expect(r.stderr).toContain('missing required family "riverstone"');
	});

	it('reports a file whose name does not match its id', () => {
		const dir = copy('misnamed');
		writeFileSync(join(dir, 'other.json'), JSON.stringify({ ...riverstone, id: 'misnamed' }));
		const r = run(dir);
		expect(r.status).toBe(1);
		expect(r.stderr).toContain('file name must be "misnamed.json"');
	});
});

describe('generated files', () => {
	const { compiled, problems } = compileDir(themesDir);

	it('compiles every family without problems', () => {
		expect(problems).toEqual([]);
		expect(compiled.map((c) => c.theme.id)).toEqual(
			expect.arrayContaining(['riverstone-light', 'riverstone-dark'])
		);
		expect(compiled).toHaveLength(48);
	});

	it('are up to date with themes/*.json (run `npm run themes`)', async () => {
		for (const [rel, text] of Object.entries(await renderOutputs(compiled))) {
			expect(readFileSync(join(root, rel), 'utf8'), rel).toBe(text);
		}
		expect(injectPrepaint(readFileSync(join(root, 'src/app.html'), 'utf8'))).toBe(
			readFileSync(join(root, 'src/app.html'), 'utf8')
		);
	});

	it('critical.css carries the header and both Riverstone blocks, near 1.5 KB gzipped-ish', () => {
		const css = readFileSync(join(root, 'src/lib/theme/critical.css'), 'utf8');
		expect(css).toMatch(/generated by scripts\/themes\.ts/);
		expect(css).toContain("[data-theme='riverstone-light']");
		expect(css).toContain("[data-theme='riverstone-dark']");
		expect(css).toContain('--color-primary: #4a7789;');
		expect(css.length).toBeLessThan(3000);
	});

	it('themes.json is the lazy list of every other theme', () => {
		const file = JSON.parse(readFileSync(join(root, 'static/themes.json'), 'utf8'));
		expect(file.v).toBe(1);
		expect(file.keys).toEqual([...TOKEN_KEYS]);
		// Every theme except Riverstone's two, which are inlined in critical.css.
		expect(file.themes).toHaveLength(46);
		expect(file.themes.map((t: { id: string }) => t.id)).not.toContain('riverstone-light');
	});
});

describe('pre-paint script', () => {
	it('is under 1 KB', () => {
		expect(Buffer.byteLength(PREPAINT)).toBeLessThan(1024);
	});

	it('is inlined in app.html and written to prepaint.js', () => {
		const html = readFileSync(join(root, 'src/app.html'), 'utf8');
		expect(html).toContain(`<script id="prepaint">${PREPAINT}</script>`);
		expect(readFileSync(join(root, 'src/lib/theme/prepaint.js'), 'utf8')).toContain(PREPAINT);
	});

	function run(opts: { stored?: unknown; raw?: string; prefersDark?: boolean; throws?: boolean }) {
		const style = new Map<string, string>();
		const el = {
			dataset: {} as Record<string, string>,
			style: {
				colorScheme: '',
				setProperty: (k: string, v: string) => void style.set(k, v)
			}
		};
		const storage = {
			getItem: (k: string) => {
				if (opts.throws) throw new Error('SecurityError');
				return k !== 'lodestar.theme'
					? null
					: opts.raw !== undefined
						? opts.raw
						: opts.stored === undefined
							? null
							: JSON.stringify(opts.stored);
			}
		};
		new Function('document', 'localStorage', 'matchMedia', PREPAINT)(
			{ documentElement: el },
			storage,
			() => ({ matches: !!opts.prefersDark })
		);
		return { theme: el.dataset.theme, scheme: el.style.colorScheme, vars: style };
	}

	it('falls back to Riverstone, following prefers-color-scheme, with nothing stored', () => {
		expect(run({})).toMatchObject({ theme: 'riverstone-light', scheme: 'light' });
		expect(run({ prefersDark: true })).toMatchObject({ theme: 'riverstone-dark', scheme: 'dark' });
	});

	it('follows prefers-color-scheme when the mode is "system"', () => {
		const stored = {
			mode: 'system',
			light: { id: 'nord-light', vars: { '--color-primary': '#5e81ac' } },
			dark: { id: 'nord-dark', vars: { '--color-primary': '#88c0d0' } }
		};
		expect(run({ stored, prefersDark: true }).theme).toBe('nord-dark');
		expect(run({ stored, prefersDark: false }).theme).toBe('nord-light');
	});

	it('an explicit mode wins over the system preference', () => {
		const stored = {
			mode: 'light',
			light: { id: 'nord-light', vars: { '--color-primary': '#5e81ac' } },
			dark: { id: 'nord-dark', vars: { '--color-primary': '#88c0d0' } }
		};
		expect(run({ stored, prefersDark: true })).toMatchObject({
			theme: 'nord-light',
			scheme: 'light'
		});
		expect(run({ stored: { mode: 'dark' }, prefersDark: false }).theme).toBe('riverstone-dark');
	});

	it('applies cached variables for a non-Riverstone theme, and only valid property names', () => {
		const stored = {
			mode: 'dark',
			dark: {
				id: 'nord-dark',
				vars: { '--color-primary': '#88c0d0', background: 'red', '--bad name': 'x' }
			}
		};
		const r = run({ stored });
		expect(r.theme).toBe('nord-dark');
		expect([...r.vars]).toEqual([['--color-primary', '#88c0d0']]);
	});

	it('falls back to Riverstone for the mode when a non-Riverstone id has no cached vars', () => {
		// data-theme='nord-dark' would match no CSS and a dark-mode user would get light colours.
		for (const vars of [undefined, {}, null, 'x']) {
			const stored = { mode: 'dark', dark: { id: 'nord-dark', vars } };
			const r = run({ stored });
			expect(r.theme, JSON.stringify(vars)).toBe('riverstone-dark');
			expect(r.scheme).toBe('dark');
			expect(r.vars.size).toBe(0);
		}
		const system = { mode: 'system', dark: { id: 'nord-dark' } };
		expect(run({ stored: system, prefersDark: true }).theme).toBe('riverstone-dark');
	});

	it('uses the resolved mode, not the stored Riverstone id', () => {
		const stored = { mode: 'dark', dark: { id: 'riverstone-light' } };
		expect(run({ stored }).theme).toBe('riverstone-dark');
	});

	it('does not override the critical CSS for Riverstone', () => {
		const stored = {
			mode: 'dark',
			dark: { id: 'riverstone-dark', vars: { '--color-primary': '#000000' } }
		};
		expect(run({ stored }).vars.size).toBe(0);
	});

	it('survives corrupt storage and bad ids', () => {
		expect(run({ raw: '{nope' }).theme).toBe('riverstone-light');
		expect(run({ raw: 'null', prefersDark: true }).theme).toBe('riverstone-dark');
		expect(run({ stored: { light: { id: 'x"><script>' } } }).theme).toBe('riverstone-light');
		expect(run({ stored: { light: { id: 5 } } }).theme).toBe('riverstone-light');
	});

	it('survives localStorage.getItem throwing', () => {
		expect(run({ throws: true })).toMatchObject({ theme: 'riverstone-light', scheme: 'light' });
		expect(run({ throws: true, prefersDark: true })).toMatchObject({
			theme: 'riverstone-dark',
			scheme: 'dark'
		});
	});
});

describe('every theme family passes the gates (docs/themes.md, 24 families)', () => {
	// docs/themes.md as of slice C1, converted to JSON. W1b generates web/themes/ from the doc.
	const families = JSON.parse(
		readFileSync(join(root, 'scripts/fixtures/families.json'), 'utf8')
	) as Family[];
	const { compiled, problems } = compileFamilies(
		families.map((family) => ({ file: `${family.id}.json`, family }))
	);

	it('has 24 valid families and compiles 48 themes', () => {
		expect(families).toHaveLength(24);
		for (const f of families) expect(validateFamily(f), f.id).toEqual([]);
		expect(problems).toEqual([]);
		expect(compiled).toHaveLength(48);
	});

	it('compiles every theme with zero gate failures', () => {
		const failing = compiled
			.filter((c) => failures(c.report).length)
			.map(
				(c) => `${c.theme.id}: ${failures(c.report).map((f) => `${f.label} ${f.value.toFixed(2)}`)}`
			);
		expect(failing).toEqual([]);
	});

	it('every label reaches 4.5:1 on its fill (7:1 for High Contrast where the fill allows)', () => {
		for (const { theme, tokens } of compiled) {
			for (const [label, fill] of [
				['primary', 'primary'],
				['info', 'info'],
				['success', 'success'],
				['warning', 'warning'],
				['error', 'error'],
				['neutral', 'neutral']
			]) {
				const c = contrast(
					tokens[`--color-${label}-content` as keyof Tokens],
					tokens[`--color-${fill}` as keyof Tokens]
				);
				expect(c, `${theme.id} ${label}-content`).toBeGreaterThanOrEqual(4.5);
			}
		}
	});

	it('text on the hover and selected tints reaches 4.5:1', () => {
		for (const { theme, tokens } of compiled) {
			for (const key of ['--color-hover', '--color-selected'] as const) {
				expect(
					contrast(tokens['--color-base-content'], tokens[key]),
					`${theme.id} ${key}`
				).toBeGreaterThanOrEqual(4.5);
			}
		}
	});

	it('a hovered row never looks selected: the tints differ by ΔE_ok ≥ 2', () => {
		for (const { theme, tokens } of compiled) {
			const gap = deltaE(tokens['--color-hover'], tokens['--color-selected']);
			expect(gap, `${theme.id}`).toBeGreaterThanOrEqual(TINT_GAP);
		}
		const solarized = compiled.find((c) => c.theme.id === 'solarized-dark')!.tokens;
		expect(
			contrast(solarized['--color-base-content'], solarized['--color-selected'])
		).toBeGreaterThanOrEqual(4.5);
	});

	it('only the documented warnings remain', () => {
		const known = /accent-content|min pairwise|error vs data-3/;
		for (const { theme, report } of compiled) {
			for (const w of report.checks.filter((c) => !c.pass && c.severity === 'warn')) {
				expect(w.label, theme.id).toMatch(known);
			}
		}
	});
});

describe('labelOn', () => {
	it('keeps the best preferred colour when it already passes', () => {
		expect(labelOn('#4A7789', ['#FFFFFF', '#F3F6F5'], '#243C44', 4.5)).toBe('#FFFFFF');
	});
	it('shifts the text colour in lightness when no preferred colour passes', () => {
		const out = labelOn('#8796A2', ['#243C44', '#FFFFFF'], '#243C44', 4.5);
		expect(out).toBe('#193038'); // Riverstone light neutral-content
		const a = toOklch(out);
		const b = toOklch('#243C44');
		expect(Math.abs(a.h - b.h)).toBeLessThan(2);
		expect(a.l).toBeLessThan(b.l);
	});
	it('goes lighter when that is the direction that reaches the target', () => {
		const out = labelOn('#2A3B45', ['#3A4A55'], '#8090A0', 4.5);
		expect(contrast(out, '#2A3B45')).toBeGreaterThanOrEqual(4.5);
		expect(toOklch(out).l).toBeGreaterThan(toOklch('#8090A0').l);
	});
	it('can reach 7:1 where the fill allows it', () => {
		const out = labelOn('#303030', ['#404040'], '#A0A0A0', 7);
		expect(contrast(out, '#303030')).toBeGreaterThanOrEqual(7);
	});
});
