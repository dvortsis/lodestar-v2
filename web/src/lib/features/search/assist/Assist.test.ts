import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import Assist from './Assist.svelte';
import { SUGGEST_DEBOUNCE_MS } from './suggest';

const json = (body: unknown) =>
	new Response(JSON.stringify(body), {
		status: 200,
		headers: { 'Content-Type': 'application/json' }
	});

let el: HTMLDivElement;
let calls: URL[];
let respond: (u: URL, signal?: AbortSignal | null) => Promise<Response>;
let accepted: [string, boolean][];
let said: string[];

const ITEMS = [
	{ kind: 'field', text: 'res:', insert: 'res:', replace: { start: 0, end: 3 } },
	{
		kind: 'chip',
		text: 'res:2160p',
		insert: 'res:2160p',
		replace: { start: 0, end: 3 },
		count: 1284
	}
];

function mount() {
	render(Assist, {
		el,
		text: () => el.textContent ?? '',
		accept: (t: string, run: boolean) => accepted.push([t, run]),
		announce: (t: string) => said.push(t)
	});
}
/** Types into the bar as the browser would: set the text, fire `input`, let the debounce pass. */
async function type(text: string) {
	el.textContent = text;
	el.dispatchEvent(new Event('input', { bubbles: true }));
	await vi.advanceTimersByTimeAsync(SUGGEST_DEBOUNCE_MS);
}
const key = (k: string, init: KeyboardEventInit = {}) => fireEvent.keyDown(el, { key: k, ...init });

beforeEach(() => {
	vi.useFakeTimers();
	el = document.createElement('div');
	el.tabIndex = 0;
	document.body.append(el);
	calls = [];
	accepted = [];
	said = [];
	respond = async () => json({ items: ITEMS });
	vi.stubGlobal(
		'fetch',
		vi.fn(async (input: string, init?: RequestInit) => {
			const u = new URL(input, 'http://x');
			if (u.pathname.endsWith('/saved-searches')) return json([]);
			calls.push(u);
			return respond(u, init?.signal);
		})
	);
});
afterEach(() => {
	vi.useRealTimers();
	vi.unstubAllGlobals();
	localStorage.clear();
	el.remove();
});

describe('Assist combobox', () => {
	it('debounces: one request for a burst of keys, with the caret in characters', async () => {
		mount();
		el.textContent = 'r';
		el.dispatchEvent(new Event('input'));
		await vi.advanceTimersByTimeAsync(SUGGEST_DEBOUNCE_MS - 20);
		await type('res');
		expect(calls).toHaveLength(1);
		expect(calls[0]!.searchParams.get('q')).toBe('res');
		expect(calls[0]!.searchParams.get('pos')).toBe('3');
		expect(await screen.findAllByRole('option')).toHaveLength(2);
		expect(el.getAttribute('aria-expanded')).toBe('true');
		expect(el.getAttribute('aria-controls')).toBe(screen.getByRole('listbox').id);
	});

	it('aborts the request in flight when the user types again, and drops its answer', async () => {
		let aborted = 0;
		respond = (u, signal) =>
			new Promise((resolve, reject) => {
				signal?.addEventListener('abort', () => {
					aborted++;
					reject(new DOMException('aborted', 'AbortError'));
				});
				if (u.searchParams.get('q') === 'resx') resolve(json({ items: [] }));
			});
		mount();
		await type('res');
		await type('resx');
		expect(aborted).toBe(1);
		expect(screen.queryByRole('option')).toBeNull();
	});

	it('moves with the arrow keys, wrapping, and reports the active option', async () => {
		mount();
		await type('res');
		await screen.findAllByRole('option');
		await key('ArrowDown');
		const [first, second] = screen.getAllByRole('option');
		expect(first!.getAttribute('aria-selected')).toBe('true');
		expect(el.getAttribute('aria-activedescendant')).toBe(first!.id);
		await key('ArrowDown');
		expect(el.getAttribute('aria-activedescendant')).toBe(second!.id);
		await key('ArrowDown');
		expect(el.getAttribute('aria-activedescendant')).toBe(first!.id);
		await key('ArrowUp');
		expect(el.getAttribute('aria-activedescendant')).toBe(second!.id);
	});

	it('Tab accepts the first option when none is active, and puts a word after a value', async () => {
		mount();
		await type('res');
		await screen.findAllByRole('option');
		await key('Tab');
		expect(accepted).toEqual([['res:', false]]); // a field keeps the caret at its colon
		expect(screen.queryByRole('listbox')).toBeNull();
		await type('res');
		await screen.findAllByRole('option');
		await key('ArrowDown');
		await key('ArrowDown');
		await key('Tab');
		expect(accepted[1]).toEqual(['res:2160p ', false]);
	});

	it('Enter accepts only an option the user moved to; otherwise it is left for the search', async () => {
		mount();
		await type('res');
		await screen.findAllByRole('option');
		const submitted = vi.fn();
		el.addEventListener('keydown', submitted); // the editor's own handler, after ours
		await key('Enter');
		expect(accepted).toEqual([]);
		expect(submitted).toHaveBeenCalledTimes(1);
		await key('ArrowDown');
		await key('Enter');
		expect(accepted).toHaveLength(1);
		expect(submitted).toHaveBeenCalledTimes(1); // the editor never saw the accepting Enter
	});

	it('Esc closes the list and keeps the text; Tab with no list moves focus on', async () => {
		mount();
		await type('res');
		await screen.findAllByRole('option');
		await key('Escape');
		expect(screen.queryByRole('listbox')).toBeNull();
		expect(el.getAttribute('aria-expanded')).toBe('false');
		expect(el.hasAttribute('aria-controls')).toBe(false);
		const tab = new KeyboardEvent('keydown', { key: 'Tab', cancelable: true, bubbles: true });
		el.dispatchEvent(tab);
		expect(tab.defaultPrevented).toBe(false);
	});

	it('shows a count with the grouping format, and says how many there are', async () => {
		mount();
		await type('res');
		expect(await screen.findByText('1,284')).toBeTruthy();
		expect(said).toEqual(['2 suggestions. Arrow keys choose, Tab accepts, Esc closes.']);
	});

	it('offers recent queries to an empty, focused bar and runs the one chosen', async () => {
		localStorage.setItem('lodestar.search.recent', JSON.stringify(['big buck bunny']));
		mount();
		el.focus();
		await waitFor(() => expect(screen.getAllByRole('option')).toHaveLength(1));
		expect(calls).toHaveLength(0); // no request for an empty bar
		await key('ArrowDown');
		await key('Enter');
		expect(accepted).toEqual([['big buck bunny', true]]);
	});

	it('runs "did you mean" as a whole-query replacement', async () => {
		respond = async () =>
			json({
				items: [
					{ kind: 'did_you_mean', text: 'batman', insert: 'batman', replace: { start: 0, end: 6 } }
				]
			});
		mount();
		await type('batmna');
		await screen.findByRole('option');
		await key('Tab');
		expect(accepted).toEqual([['batman', true]]);
	});

	it('never accepts an answer meant for older text', async () => {
		let release: ((r: Response) => void) | undefined;
		respond = (u) =>
			u.searchParams.get('q') === 'res'
				? new Promise((resolve) => (release = resolve))
				: Promise.resolve(json({ items: [] }));
		mount();
		await type('res'); // the request leaves and waits
		el.textContent = 'resx';
		el.dispatchEvent(new Event('input')); // typed on inside the debounce window
		release?.(json({ items: ITEMS }));
		await vi.advanceTimersByTimeAsync(0);
		expect(screen.queryByRole('option')).toBeNull();
		await key('Tab');
		expect(accepted).toEqual([]);
	});

	it('Tab leaves an untouched bar alone, even with recent queries showing', async () => {
		localStorage.setItem('lodestar.search.recent', JSON.stringify(['big buck bunny']));
		mount();
		el.focus();
		await waitFor(() => expect(screen.getAllByRole('option')).toHaveLength(1));
		const tab = new KeyboardEvent('keydown', { key: 'Tab', cancelable: true, bubbles: true });
		el.dispatchEvent(tab);
		expect(tab.defaultPrevented).toBe(false);
		expect(accepted).toEqual([]);
	});

	it('Enter that searches closes the list, so the next Tab leaves the bar', async () => {
		mount();
		await type('res');
		await screen.findAllByRole('option');
		await key('Enter'); // no option chosen: left for the search
		expect(screen.queryByRole('listbox')).toBeNull();
		const tab = new KeyboardEvent('keydown', { key: 'Tab', cancelable: true, bubbles: true });
		el.dispatchEvent(tab);
		expect(tab.defaultPrevented).toBe(false);
		expect(accepted).toEqual([]);
	});

	it('Enter before the answer lands: the late answer does not open the list', async () => {
		let release: ((r: Response) => void) | undefined;
		respond = () => new Promise((resolve) => (release = resolve));
		mount();
		await type('res'); // asked, not answered
		await key('Enter');
		release?.(json({ items: ITEMS }));
		await vi.advanceTimersByTimeAsync(SUGGEST_DEBOUNCE_MS);
		expect(screen.queryByRole('listbox')).toBeNull();
		const tab = new KeyboardEvent('keydown', { key: 'Tab', cancelable: true, bubbles: true });
		el.dispatchEvent(tab);
		expect(tab.defaultPrevented).toBe(false);
	});

	it('shows no list when the request fails', async () => {
		respond = async () => new Response('{}', { status: 500 });
		mount();
		await type('res');
		expect(screen.queryByRole('listbox')).toBeNull();
	});
});
