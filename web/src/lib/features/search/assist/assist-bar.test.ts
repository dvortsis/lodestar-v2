// Autocomplete, smart chips, natural phrasing and the Builder inside the real query bar, against a
// small fake server.
import { fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createSearchState, type Ast } from '$lib/state/search.svelte';

vi.mock('$app/state', async () => ({ page: (await import('../query/testing.svelte')).testPage }));
vi.mock('$app/navigation', () => ({ goto: vi.fn(), replaceState: vi.fn() }));

import QueryBar from '../query/QueryBar.svelte';
import { testNav, visit } from '../query/testing.svelte';

const json = (body: unknown, status = 200) =>
	new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } });

const word = (w: string): Ast => ({ op: 'word', field: 'any', word: w });
const res = (v: string): Ast => ({ op: 'tag', key: 'res', any: [v] });
const toAst = (q: string): Ast => {
	const of = q
		.split(/\s+/)
		.filter(Boolean)
		.map((w) => (w.startsWith('res:') ? res(w.slice(4)) : word(w)));
	return of.length === 0 ? { op: 'all' } : of.length === 1 ? of[0]! : { op: 'and', of };
};
const print = (ast: Ast): string =>
	(ast.op === 'and' ? ast.of : ast.op === 'all' ? [] : [ast])
		.map((n) => (n.op === 'word' ? n.word : n.op === 'tag' ? `res:${n.any[0]}` : '?'))
		.join(' ');

let requests: URL[];
let searchBodies: { ast: Ast }[];
function server() {
	requests = [];
	searchBodies = [];
	vi.stubGlobal(
		'fetch',
		vi.fn(async (input: string, init?: RequestInit) => {
			const u = new URL(input, 'http://x');
			requests.push(u);
			if (u.pathname.endsWith('/suggest'))
				return json({
					items: [
						{
							kind: 'chip',
							text: 'res:2160p',
							insert: 'res:2160p',
							replace: { start: 0, end: 5 },
							count: 1284
						}
					]
				});
			if (u.pathname.endsWith('/saved-searches')) return json([]);
			if (u.pathname.endsWith('/search')) {
				const body = JSON.parse(String(init?.body)) as { ast: Ast };
				searchBodies.push(body);
				return json({ canonical: print(body.ast), hits: [], total: 0 });
			}
			const q = u.searchParams.get('q') ?? '';
			const base = { ast: toAst(q), canonical: q.trim() };
			if (u.searchParams.get('natural') === 'true')
				return json(
					q.includes('4k')
						? {
								...base,
								natural: {
									ast: toAst('res:2160p movies'),
									canonical: 'res:2160p movies',
									chips: [{ from: { start: 0, end: 2 }, text: 'res:2160p' }],
									text: ['movies']
								}
							}
						: base
				);
			return json(base);
		})
	);
}

beforeEach(() => {
	visit('/search');
	server();
});
afterEach(() => {
	vi.unstubAllGlobals();
	localStorage.clear();
});

const LAZY = { timeout: 10_000 };

async function setup(q?: string) {
	if (q) visit(`/search?q=${encodeURIComponent(q)}`);
	const state = createSearchState(testNav);
	render(QueryBar, { searchState: state });
	const editor = screen.getByRole('combobox', { name: 'Search torrents' });
	await fireEvent.focusIn(editor);
	editor.focus();
	await waitFor(() => expect(screen.getByRole('button', { name: 'Builder' })).toBeTruthy(), LAZY);
	return { state, editor };
}
async function type(editor: HTMLElement, text: string) {
	editor.textContent = text;
	await fireEvent(
		editor,
		new InputEvent('input', { inputType: 'insertText', data: text.slice(-1) })
	);
}

describe('smart chips and autocomplete in the bar', () => {
	it('offers res:2160p for 2160p and Tab rewrites the query through the search state', async () => {
		const { state, editor } = await setup();
		await type(editor, '2160p');
		await screen.findByRole('option', { name: /res:2160p/ }, LAZY);
		await fireEvent.keyDown(editor, { key: 'Tab' });
		await waitFor(() => expect(editor.textContent).toBe('res:2160p '), LAZY);
		await waitFor(() => expect(state.ast).toEqual(res('2160p')), LAZY);
	});

	it('records a searched query in the recent list, and offers it to an empty bar', async () => {
		const { editor } = await setup();
		await type(editor, 'big buck bunny');
		await screen.findByRole('listbox', {}, LAZY); // autocomplete has loaded
		await fireEvent.keyDown(editor, { key: 'Escape' });
		await fireEvent.keyDown(editor, { key: 'Enter' });
		await waitFor(
			() => expect(localStorage.getItem('lodestar.search.recent')).toContain('big buck bunny'),
			LAZY
		);
		await type(editor, '');
		await fireEvent.focus(editor);
		await screen.findByRole('option', { name: /big buck bunny/ }, LAZY);
	}, 30_000);
});

describe('natural phrasing', () => {
	it('previews the chips and changes nothing until Apply', async () => {
		const { state, editor } = await setup();
		await type(editor, '4k movies');
		await fireEvent.click(screen.getByRole('button', { name: 'Natural phrasing' }));
		const panel = await screen.findByRole('region', { name: 'Natural phrasing preview' }, LAZY);
		expect(await within(panel).findByText('res:2160p', {}, LAZY)).toBeTruthy();
		expect(within(panel).getByText('Kept as words: movies')).toBeTruthy();
		expect(editor.textContent).toBe('4k movies');
		await waitFor(() =>
			expect(document.activeElement).toBe(within(panel).getByRole('button', { name: 'Apply' }))
		);
		await fireEvent.click(within(panel).getByRole('button', { name: 'Apply' }));
		await waitFor(() => expect(editor.textContent).toBe('res:2160p movies'), LAZY);
		await waitFor(() => expect(state.text).toBe('res:2160p movies'), LAZY);
		expect(screen.queryByRole('region', { name: 'Natural phrasing preview' })).toBeNull();
	});

	it('Dismiss and Esc leave the text alone and return focus to the bar', async () => {
		const { editor } = await setup();
		await type(editor, '4k movies');
		await fireEvent.click(screen.getByRole('button', { name: 'Natural phrasing' }));
		const panel = await screen.findByRole('region', { name: 'Natural phrasing preview' }, LAZY);
		await fireEvent.click(await within(panel).findByRole('button', { name: 'Dismiss' }, LAZY));
		expect(screen.queryByRole('region', { name: 'Natural phrasing preview' })).toBeNull();
		expect(editor.textContent).toBe('4k movies');
		await waitFor(() => expect(document.activeElement).toBe(editor));
	});

	it('Esc closes the preview too', async () => {
		const { editor } = await setup();
		await type(editor, '4k movies');
		await fireEvent.click(screen.getByRole('button', { name: 'Natural phrasing' }));
		const panel = await screen.findByRole('region', { name: 'Natural phrasing preview' }, LAZY);
		await fireEvent.keyDown(panel, { key: 'Escape' });
		expect(screen.queryByRole('region', { name: 'Natural phrasing preview' })).toBeNull();
		expect(editor.textContent).toBe('4k movies');
	});

	it('says so when nothing became a filter', async () => {
		const { editor } = await setup();
		await type(editor, 'just words');
		await fireEvent.click(screen.getByRole('button', { name: 'Natural phrasing' }));
		expect(await screen.findByText(/Nothing in this text became a filter/, {}, LAZY)).toBeTruthy();
		expect(screen.queryByRole('button', { name: 'Apply' })).toBeNull();
	});
});

describe('the Builder', () => {
	it('shows the query as rows, and an edited row goes back through the server AST to the bar', async () => {
		const { state, editor } = await setup('bunny res:720p');
		await waitFor(
			() => expect(state.ast).toEqual({ op: 'and', of: [word('bunny'), res('720p')] }),
			LAZY
		);
		await fireEvent.click(screen.getByRole('button', { name: 'Builder' }));
		const panel = await screen.findByRole('region', { name: 'Query builder' }, LAZY);
		const row = within(panel).getByRole('group', { name: 'Resolution: 720p' });
		const value = within(row).getByLabelText('Value') as HTMLInputElement;
		expect(value.value).toBe('720p');
		await fireEvent.input(value, { target: { value: '1080p' } });
		await fireEvent.click(within(row).getByRole('button', { name: 'Apply' }));
		await waitFor(() => expect(editor.textContent).toBe('bunny res:1080p'), LAZY);
		expect(searchBodies.at(-1)!.ast).toEqual({ op: 'and', of: [word('bunny'), res('1080p')] });
		expect(requests.some((u) => u.searchParams.get('q') === 'res:1080p')).toBe(true);
	});

	it('typing in the bar updates the rows, and a new row appends a clause', async () => {
		const { editor } = await setup();
		await fireEvent.click(screen.getByRole('button', { name: 'Builder' }));
		const panel = await screen.findByRole('region', { name: 'Query builder' }, LAZY);
		await type(editor, 'res:720p');
		await waitFor(() => expect(within(panel).getAllByLabelText('Value')).toHaveLength(2), LAZY);
		const draft = within(panel).getByRole('group', { name: 'New filter' });
		await fireEvent.input(within(draft).getByLabelText('Value'), { target: { value: '2160p' } });
		await fireEvent.click(within(draft).getByRole('button', { name: 'Add filter' }));
		await waitFor(() => expect(editor.textContent).toBe('res:720p res:2160p'), LAZY);
	});

	it('refuses a value that reads as several clauses', async () => {
		const { editor } = await setup('res:720p');
		await fireEvent.click(screen.getByRole('button', { name: 'Builder' }));
		const panel = await screen.findByRole('region', { name: 'Query builder' }, LAZY);
		const draft = within(panel).getByRole('group', { name: 'New filter' });
		await fireEvent.input(within(draft).getByLabelText('Value'), { target: { value: 'a b' } });
		await fireEvent.click(within(draft).getByRole('button', { name: 'Add filter' }));
		expect(await within(panel).findByText(/Couldn't apply this filter/, {}, LAZY)).toBeTruthy();
		expect(editor.textContent).toBe('res:720p');
	});

	it('asks for a value, and removes a row', async () => {
		await setup('res:720p');
		await fireEvent.click(screen.getByRole('button', { name: 'Builder' }));
		const panel = await screen.findByRole('region', { name: 'Query builder' }, LAZY);
		const draft = within(panel).getByRole('group', { name: 'New filter' });
		await fireEvent.click(within(draft).getByRole('button', { name: 'Add filter' }));
		expect(await within(panel).findByText(/Enter a value/, {}, LAZY)).toBeTruthy();
		await fireEvent.click(
			within(panel).getByRole('button', { name: 'Remove filter: Resolution: 720p' })
		);
		await waitFor(() => expect(searchBodies.at(-1)!.ast).toEqual({ op: 'all' }), LAZY);
	});
});
