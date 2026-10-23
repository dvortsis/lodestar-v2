// Shared bits of the chart primitives: the fixed category order and its data tokens
// (docs/design-tokens.md §3), and the geometry helpers that keep every chart honest about nulls.
import type { MetricValue } from '$lib/format';

export type Group = 'video' | 'audio' | 'images' | 'documents' | 'archives' | 'software' | 'other';

/** Fixed segment order (§10.18): Video, Audio, Images, Documents, Archives, Software, Other. */
export const GROUP_ORDER: readonly Group[] = [
	'video',
	'audio',
	'images',
	'documents',
	'archives',
	'software',
	'other'
];

/** Slot number = category group, identical everywhere (design-tokens.md §3). */
export const GROUP_COLOR: Record<Group, string> = {
	video: 'var(--data-1)',
	audio: 'var(--data-2)',
	images: 'var(--data-5)',
	documents: 'var(--data-4)',
	archives: 'var(--data-6)',
	software: 'var(--data-3)',
	other: 'var(--data-other)'
};

/** A formatter from `$lib/format`, bound by the caller to the unit it shows. */
export type ValueFormatter = (value: MetricValue, locale?: string) => string;

/** The plain number behind a `MetricValue`, or null when it is unknown. */
export function numeric(value: MetricValue): number | null {
	const n = typeof value === 'object' && value !== null ? value.value : value;
	return typeof n === 'number' && Number.isFinite(n) ? n : null;
}

/** `part / whole` as a CSS percentage for a bar's length; never NaN, clamped to 0…100. */
export function lengthPct(part: number | null, whole: number): string {
	if (part === null || !(whole > 0) || !(part > 0)) return '0%';
	return `${Math.min(100, (part / whole) * 100)}%`;
}
