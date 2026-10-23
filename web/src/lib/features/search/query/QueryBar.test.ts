import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createSearchState, type Ast } from '$lib/state/search.svelte';

vi.mock('$app/state', async () => ({ page: (await import('./testing.svelte')).testPage }));
const replaceState = vi.hoisted(() => vi.fn());
vi.mock('$app/navigation', () => ({ goto: vi.fn(), replaceState }));

import appHtml from '../../../../app.html?raw';
import { m } from '$lib/paraglide/messages.js';
import QueryBar from './QueryBar.svelte';
import { navCalls, testNav, visit } from './testing.svelte';

const json = (body: unknown, status = 200) =>
	new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } });

const word = (w: string): Ast => ({ op: 'word', field: 'any', word: w });
const res = (v: string): Ast => ({ op: 'tag', key: 'res', any: [v] });

/** The fake server: `(` is a syntax error at its byte; "down" fails the network; `res:` parses. */
let fetchMock: ReturnType<typeof vi.fn>;
let searchBodies: unknown[];
function server() {
	searchBodies = [];
	fetchMock = vi.fn(async (input: string, init?: RequestInit) => {
		const u = new URL(input, 'http://x');
		if (u.pathname.endsWith('/search')) {
			const body = JSON.parse(String(init?.body));
			searchBodies.push(body);
			const tops: Ast[] = body.ast.op === 'and' ? body.ast.of : [body.ast];
			const canonical = tops
				.map((n) => (n.op === 'word' ? n.word : n.op === 'tag' ? `res:${n.any[0]}` : ''))
				.join(' ');
			return json({ canonical, hits: [], total: 0 });
		}
		const q = u.searchParams.get('q') ?? '';
		if (q.includes('down')) throw new TypeError('Failed to fetch');
		const at = new TextEncoder().encode(q).indexOf(40); // "("
		if (at >= 0)
			return json(
				{
					error: {
						code: 'query_syntax',
						message: "missing ')'",
						field: 'q',
						start: at,
						end: at + 1
					}
				},
				400
			);
		const of = q
			.split(/\s+/)
			.filter(Boolean)
			.map((w) => (w.startsWith('res:') ? res(w.slice(4)) : word(w)));
		const ast: Ast = of.length === 0 ? { op: 'all' } : of.length === 1 ? of[0]! : { op: 'and', of };
		return json({ ast, canonical: q.trim() });
	});
	vi.stubGlobal('fetch', fetchMock);
}

beforeEach(() => {
	visit('/search');
	server();
	replaceState.mockReset();
});
afterEach(() => vi.unstubAllGlobals());

function setup() {
	const state = createSearchState(testNav);
	render(QueryBar, { searchState: state });
	const editor = screen.getByRole('combobox', { name: 'Search torrents' });
	return { state, editor };
}

/** Loads the lazy part the way a user does: focus. */
async function engage(editor: HTMLElement) {
	await fireEvent.focusIn(editor);
	editor.focus();
	// Live's message line replaces the static one.
	await waitFor(() =>
		expect(document.querySelector('#search-query-message[data-live]')).not.toBeNull()
	);
}

async function typeText(editor: HTMLElement, text: string) {
	editor.textContent = text;
	await fireEvent(
		editor,
		new InputEvent('input', { inputType: 'insertText', data: text.slice(-1) })
	);
}

const message = () => document.getElementById('search-query-message')!;

describe('the query bar', () => {
	it('is a labelled combobox with the attributes typing needs', () => {
		const { editor } = setup();
		expect(editor.getAttribute('contenteditable')).toBe('plaintext-only');
		expect(editor).toHaveProperty('tabIndex', 0);
		for (const [name, value] of [
			['aria-expanded', 'false'],
			['aria-autocomplete', 'list'],
			['aria-haspopup', 'listbox'],
			['aria-describedby', 'search-query-message'],
			['spellcheck', 'false'],
			['autocapitalize', 'off'],
			['autocorrect', 'off'],
			['enterkeyhint', 'search'],
			['inputmode', 'search']
		])
			expect(editor.getAttribute(name!), name).toBe(value);
		expect(editor.hasAttribute('aria-invalid')).toBe(false);
		// The placeholder is never the accessible name.
		const placeholder = screen.getByText('Search by name, or add a filter such as res:1080p');
		expect(placeholder.closest('[aria-hidden="true"]')).not.toBeNull();
		expect(screen.getByText('Search, or filter: res:1080p').parentElement).toBe(
			placeholder.parentElement
		);
		// The search landmark (jsdom has no implicit role for <search>; the e2e spec checks the role).
		const landmark = editor.closest('search')!;
		expect(document.getElementById(landmark.getAttribute('aria-labelledby')!)?.textContent).toBe(
			'Search torrents'
		);
	});

	it('fetches nothing for the bare page', async () => {
		const { editor } = setup();
		await engage(editor);
		expect(fetchMock).not.toHaveBeenCalled();
	});

	it('parses a URL query once and shows it in the bar', async () => {
		visit('/search?q=bunny');
		const { editor } = setup();
		await waitFor(() => expect(editor.textContent).toBe('bunny'));
		expect(fetchMock.mock.calls.map((c) => c[0])).toEqual(['/api/v1/parse?q=bunny']);
	});

	it('typing calls setText and Enter submits', async () => {
		const { state, editor } = setup();
		await engage(editor);
		const setText = vi.spyOn(state, 'setText');
		const submit = vi.spyOn(state, 'submit');
		await typeText(editor, 'big buck');
		expect(setText).toHaveBeenLastCalledWith('big buck');
		await fireEvent.keyDown(editor, { key: 'Enter' });
		expect(submit).toHaveBeenCalledOnce();
		await waitFor(() => expect(navCalls.at(-1)).toEqual({ url: '/search?q=big+buck', push: true }));
	});

	it('keeps what was typed and the Enter pressed before the editor loaded', async () => {
		const { editor } = setup();
		editor.textContent = 'res:4k';
		await fireEvent.keyDown(editor, { key: 'Enter' }); // no listener yet but the static one
		await engage(editor);
		await waitFor(() => expect(navCalls.at(-1)).toEqual({ url: '/search?q=res%3A4k', push: true }));
		expect(editor.querySelector('.qt-field')?.textContent).toBe('res:');
	});

	it('shows a syntax error under the bar and marks it, then clears it on the next edit', async () => {
		const { editor } = setup();
		await engage(editor);
		await typeText(editor, 'a ( b');
		await fireEvent.keyDown(editor, { key: 'Enter' });
		await waitFor(() => expect(editor.getAttribute('aria-invalid')).toBe('true'));
		expect(message().textContent).toContain("Can't read this query:");
		expect(message().textContent).toContain("missing ')'");
		expect(editor.querySelector('.qt-error')?.textContent).toBe('(');
		await typeText(editor, 'a ( bc');
		expect(editor.hasAttribute('aria-invalid')).toBe(false);
		expect(editor.querySelector('.qt-error')).toBeNull();
	});

	it('shows a banner, not the message line, when the check itself fails', async () => {
		const { state, editor } = setup();
		await engage(editor);
		await typeText(editor, 'down');
		await fireEvent.keyDown(editor, { key: 'Enter' });
		const banner = await screen.findByRole('alert');
		expect(banner.textContent).toContain("Couldn't check the query");
		expect(editor.hasAttribute('aria-invalid')).toBe(false);
		const submit = vi.spyOn(state, 'submit');
		await fireEvent.click(screen.getByRole('button', { name: 'Retry' }));
		expect(submit).toHaveBeenCalledOnce();
	});

	it('never shows an error for an empty query', async () => {
		const { state, editor } = setup();
		await engage(editor);
		fetchMock.mockImplementation(async () => {
			throw new TypeError('Failed to fetch');
		});
		await fireEvent.keyDown(editor, { key: 'Enter' });
		// The empty query's parse has failed: the state holds the error, the page shows none.
		await waitFor(() => expect(state.parseError).not.toBeNull());
		expect(state.parsing).toBe(false);
		expect(screen.queryByRole('alert')).toBeNull();
		expect(message().textContent?.trim()).toBe('');
	});

	it('reserves the message line from first paint and keeps it one element', async () => {
		const { editor } = setup();
		const before = message();
		expect(before.className).toContain('text-small');
		expect(before.textContent).toBe(' '); // holds the line before anything loads
		expect(before.getAttribute('role')).toBe('status'); // a polite live region (WCAG 4.1.3)
		await engage(editor);
		expect(message().className).toContain('text-small');
		expect(message().textContent).toBe('\u00a0'); // still one empty line once loaded
		await typeText(editor, '(');
		await fireEvent.keyDown(editor, { key: 'Enter' });
		await waitFor(() => expect(message().textContent).toContain("missing ')'"));
		expect(document.querySelectorAll('#search-query-message')).toHaveLength(1);
		expect(message().getAttribute('role')).toBe('status');
		// An error wraps rather than being cut (WCAG 1.4.10): no truncation.
		expect(message().className).not.toContain('truncate');
	});

	it('puts the URL back on /search before writing (the drawer is open on /t/…)', async () => {
		visit('/t/abc?q=a', { detail: {} });
		const { editor } = setup();
		await engage(editor);
		await typeText(editor, 'a b');
		expect(replaceState).toHaveBeenCalledWith('/search?q=a', {});
	});

	it('shows the URL query in the bar (a pasted link, back)', async () => {
		visit('/search?q=res%3A1080p');
		const { editor } = setup();
		await waitFor(() => expect(editor.textContent).toBe('res:1080p'));
		expect(editor.querySelector('.qt-value')?.textContent).toBe('1080p');
	});
});

describe('the static first-paint bar (app.html)', () => {
	it('shows the same placeholders as the app’s bar', () => {
		// The app's bar replaces it in place, so the two must read the same.
		expect(appHtml).toContain(`>${m.search_query_placeholder_short()}</span`);
		expect(appHtml).toMatch(new RegExp(`>\\s*${m.search_query_placeholder()}</span`));
	});
});
