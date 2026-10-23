// How a value reads in a sentence ("Changed elsewhere to Aggressive", "Automatic · 400") and
// what a preview line says. Numbers go through `$lib/format` (standards §4).
import { formatBytes, formatCount } from '$lib/format';
import { currentLocale } from '$lib/i18n/locale.svelte';
import { m } from '$lib/paraglide/messages.js';
import { settingText, unitText } from './messages';
import type { FieldSpec, FieldState, PreviewEstimate } from './types';

const MIB = 1_048_576;

/** An enum value's label: `settings.<section>.<field>.option.<value>`, else the value. */
export function optionLabel(spec: FieldSpec, value: string): string {
	return settingText(`settings.${spec.section}.${spec.name}.option.${value}`) ?? value;
}

/** A value as a short phrase, for conflicts, Automatic and read-only fields. */
export function displayValue(spec: FieldSpec, value: unknown): string {
	if (value === null || value === undefined || value === '') return '—';
	if (typeof value === 'boolean') return value ? m.settings_form_on() : m.settings_form_off();
	if (typeof value === 'number') {
		const n = formatCount(value, currentLocale());
		const unit = unitText(spec.unit);
		return unit ? `${n} ${unit}` : n;
	}
	if (typeof value === 'string') return spec.kind === 'enum' ? optionLabel(spec, value) : value;
	if (Array.isArray(value) && value.every((v) => typeof v === 'string')) return value.join(', ');
	return JSON.stringify(value);
}

export type EstimatePart = { text: string; mono?: string; testid: string };

/** One preview estimate as text (`url` puts the address apart, for Mono). */
export function estimatePart(e: PreviewEstimate): EstimatePart | undefined {
	const locale = currentLocale();
	if (e.name === 'sockets' && typeof e.value === 'number') {
		return {
			text: m.settings_form_estimate_sockets({ count: formatCount(e.value, locale) }),
			testid: 'estimate-sockets'
		};
	}
	if (e.name === 'memory_mib' && typeof e.value === 'number') {
		return {
			text: m.settings_form_estimate_memory({ size: formatBytes(e.value * MIB, locale) }),
			testid: 'estimate-memory'
		};
	}
	if (e.name === 'url' && typeof e.value === 'string') {
		return { text: m.settings_form_estimate_url(), mono: e.value, testid: 'estimate-url' };
	}
	return undefined; // an estimate this UI doesn't know yet
}

/** The effective value behind "Automatic · 400": the field's one estimate, when it has one. */
export function automaticValue(f: FieldState): string | undefined {
	const estimates = f.preview?.estimates ?? [];
	if (estimates.length !== 1) return undefined;
	const v = estimates[0]!.value;
	return typeof v === 'number' ? formatCount(v, currentLocale()) : undefined;
}

/** The number behind "Automatic · 400", to pre-fill the input when Automatic is turned off. */
export function automaticNumber(f: FieldState): number | undefined {
	const estimates = f.preview?.estimates ?? [];
	const v = estimates.length === 1 ? estimates[0]!.value : undefined;
	return typeof v === 'number' ? v : undefined;
}
