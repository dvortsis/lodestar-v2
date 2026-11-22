// Movement's annotation notes (X-pseudo-text-gaps): an intensity change shows the level's label
// (Insights' own message, with the same words as Settings), and a level or kind this build doesn't
// know is a sentence of its own, never the server's raw English word inserted into a translated
// one. Both locales.
import { cleanup, render, screen } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { initLocale } from '$lib/i18n/locale.svelte';
import { overviewFixture } from './fixtures';
import Movement from './Movement.svelte';
import type { Overview } from './model';
import type { InsightsQuery } from './query';

const q: InsightsQuery = { range: '7d', compare: false };
const HOUR = 3600;

beforeEach(() => {
	vi.stubGlobal(
		'ResizeObserver',
		class {
			observe() {}
			disconnect() {}
			unobserve() {}
		}
	);
});
afterEach(() => {
	cleanup();
	vi.unstubAllGlobals();
	initLocale('en');
});

function notes(annotations: Overview['annotations'], locale: 'en' | 'zh') {
	initLocale(locale);
	const o = overviewFixture();
	render(Movement, { overview: { ...o, annotations }, query: q, rangeText: 'x' });
	return annotations.map((a) => {
		const li = screen.getAllByTestId(`insights-annotation-${a.kind}`)[0]!;
		return li.querySelector('span')!.textContent!.trim();
	});
}

const at = overviewFixture().annotations[0]!.at;
const note = (kind: string, text: string): Overview['annotations'][number] => ({
	at: at + HOUR,
	kind,
	text,
	import: null
});

describe('Movement notes', () => {
	it("an intensity change shows the level's label, with the same words as Settings", () => {
		expect(notes([note('intensity', 'aggressive')], 'en')).toEqual([
			'Intensity changed to Aggressive'
		]);
		cleanup();
		expect(notes([note('intensity', 'low')], 'zh')).toEqual(['强度已更改为低']);
	});

	it('a level this build does not know is not inserted', () => {
		expect(notes([note('intensity', 'turbo')], 'en')).toEqual(['Intensity changed']);
		cleanup();
		const [zh] = notes([note('intensity', 'turbo')], 'zh');
		expect(zh).toBe('强度已更改');
		expect(zh).not.toMatch(/turbo/);
	});

	it('a name off the object prototype is unknown too', () => {
		expect(notes([note('intensity', 'constructor')], 'en')).toEqual(['Intensity changed']);
		cleanup();
		expect(notes([note('constructor', '')], 'en')).toEqual(['An unrecognised event']);
	});

	it('an unknown kind reads as a sentence, with no raw text', () => {
		const [en] = notes([note('import_rewound', 'job=3')], 'en');
		expect(en).toBe('An unrecognised event');
		cleanup();
		const [zh] = notes([note('import_rewound', 'job=3')], 'zh');
		expect(zh).toBe('无法识别的事件');
		for (const s of [en, zh]) expect(s).not.toMatch(/import|rewound|job/);
	});
});
