import { afterEach, describe, expect, it, vi } from 'vitest';
import { createRawSnippet } from 'svelte';
import { fireEvent, render, screen } from '@testing-library/svelte';
import ResultRow from './ResultRow.svelte';
import type { SearchHit } from './ResultRow.svelte';
import { ROW_FILTER } from './badges';
import { hit } from './fixtures';

const actions = createRawSnippet<[SearchHit]>(() => ({ render: () => '<span></span>' }));

describe('ResultRow: click a badge to filter (§4c)', () => {
	afterEach(() => document.body.removeEventListener('click', opened));
	let opened = vi.fn();
	const sent = vi.fn();
	const on = (e: Event) => sent((e as CustomEvent).detail);
	window.addEventListener(ROW_FILTER, on);
	afterEach(() => sent.mockClear());

	it('sends the facet and value, and does not open the row', async () => {
		opened = vi.fn();
		render(ResultRow, {
			hit: hit(1, { content_type: 'movie', resolution: '1080p' }),
			density: 'comfortable',
			testid: 'r',
			actions
		});
		document.body.addEventListener('click', opened);
		await fireEvent.click(screen.getByTestId('r-badge-res'));
		expect(sent).toHaveBeenCalledWith({ facet: 'res', value: '1080p' });
		expect(opened).not.toHaveBeenCalled();
	});

	it('the +N chip does open the row (its click bubbles to the list)', async () => {
		opened = vi.fn();
		render(ResultRow, {
			hit: hit(1, {
				content_type: 'movie',
				resolution: '1080p',
				flags: ['private', 'imported', 'v2', 'hybrid']
			}),
			density: 'comfortable',
			testid: 'r',
			actions
		});
		document.body.addEventListener('click', opened);
		await fireEvent.click(screen.getByTestId('r-badges-more'));
		expect(opened).toHaveBeenCalledTimes(1);
		expect(sent).not.toHaveBeenCalled();
	});
});
