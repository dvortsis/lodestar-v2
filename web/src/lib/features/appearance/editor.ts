// The custom theme editor's engine (§10.18): compiles an edited palette with the build's own theme
// compiler (scripts/themes-core.ts), so the live report and the shipped themes use one set of rules.
// Loaded only with the editor.
import {
	TOKEN_KEYS,
	derive,
	evaluate,
	expandFamily,
	failures,
	validateFamily,
	warnings,
	type Family,
	type Palette,
	type Report
} from '../../../../scripts/themes-core.ts';
import riverstone from '../../../../themes/riverstone.json';
import type { ThemeEntry, ThemesFile } from './themes';

export type { Family, Palette, Report };
export const COLOUR_FIELDS = ['background', 'surface', 'text', 'primary', 'accent'] as const;
export const HEX = /^#[0-9a-fA-F]{6}$/;
export const START = riverstone as unknown as Family;

export interface Compiled {
	report: Report;
	failed: string[];
	warned: string[];
	entry: ThemeEntry;
}

/** One mode's palette → tokens, the contrast report and the picker entry. */
export function compile(family: Family, mode: 'light' | 'dark'): Compiled {
	const theme = expandFamily(family)[mode === 'light' ? 0 : 1]!;
	const tokens = derive(theme);
	const report = evaluate(theme, tokens);
	return {
		report,
		failed: failures(report).map((c) => c.label),
		warned: warnings(report).map((c) => c.label),
		entry: {
			id: theme.id,
			name: theme.name,
			family: theme.family,
			group: 'original',
			mode,
			contrast: report.contrast,
			values: TOKEN_KEYS.map((k) => tokens[k].toLowerCase()),
			custom: true
		}
	};
}

/** The server keeps a theme's id: lowercase words joined by "-" (preferences.rs `theme_id`). */
export function slug(name: string, taken: string[]): string {
	const base =
		'custom-' +
		(name
			.toLowerCase()
			.replace(/[^a-z0-9]+/g, '-')
			.replace(/^-|-$/g, '')
			.slice(0, 40) || 'theme');
	let id = base;
	for (let n = 2; taken.includes(id); n++) id = `${base}-${n}`;
	return id;
}

/** A family's base palette, rebuilt from its compiled tokens (themes.json holds no base palette). */
export function paletteOf(file: ThemesFile, entry: ThemeEntry): Palette {
	const v = (key: string) => entry.values[file.keys.indexOf(key)] ?? '';
	return {
		background: v('--color-base-200'),
		surface: v('--color-base-100'),
		text: v('--color-base-content'),
		primary: v('--brand-primary'),
		accent: v('--brand-accent'),
		categorical: [1, 5, 2, 3, 4, 6].map((slot) => v(`--data-${slot}`))
	};
}

/** Reads an imported family file; errors name the first problems. */
export function parseImport(text: string): { family?: Family; errors: string[] } {
	let raw: unknown;
	try {
		raw = JSON.parse(text);
	} catch {
		return { errors: ['not JSON'] };
	}
	const errors = validateFamily(raw);
	return errors.length ? { errors: errors.slice(0, 3) } : { family: raw as Family, errors };
}
