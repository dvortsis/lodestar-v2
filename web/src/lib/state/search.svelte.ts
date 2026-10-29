// Search state ⇄ URL (§10.4). The URL holds the server's canonical query text (`q`) plus `sort`,
// `hidden` and `exact`; the AST always comes from the server (`/parse`, or the `canonical` of a
// `/search` that was sent an AST). The client never parses or prints queries itself.
//
// History: typing replaces the entry (after the debounce); submit, structured edits, option
// changes and reset push one, so back and forward walk searches. `sync()` restores the draft
// from the URL when it changed from outside (back/forward, a pasted link).
import { goto } from '$app/navigation';
import { page } from '$app/state';
import { ApiError, api, isAbort, latest } from '$lib/api/client';
import type { components } from '$lib/api/schema';

export type Ast = components['schemas']['AstNode'];
export type ParseError = Pick<
	components['schemas']['ErrorBody'],
	'code' | 'message' | 'start' | 'end'
>;

export const DEFAULT_SORT = 'relevance';
export const PARSE_DEBOUNCE_MS = 200;
const SORT = /^(relevance|[+-]?(discovered|size|files))$/;

export type UrlState = {
	q: string;
	sort: string;
	hidden: boolean;
	exact: boolean;
	browse?: boolean;
};

const flag = (v: string | null) => v === '1' || v === 'true';

/** Reads the URL; an invalid value falls back to its default. */
export function readUrl(params: URLSearchParams): UrlState {
	const sort = params.get('sort') ?? DEFAULT_SORT;
	return {
		q: params.get('q') ?? '',
		sort: SORT.test(sort) ? sort : DEFAULT_SORT,
		hidden: flag(params.get('hidden')),
		exact: flag(params.get('exact')),
		browse: params.get('browse') === '1'
	};
}

/** The URL for a state: defaults are omitted, and every other parameter is kept as it was. */
export function writeUrl(base: URL, s: UrlState): string {
	// A local builder, never stored or rendered.
	// eslint-disable-next-line svelte/prefer-svelte-reactivity
	const u = new URL(base);
	const set = (k: string, v: string | null) =>
		v === null ? u.searchParams.delete(k) : u.searchParams.set(k, v);
	set('q', s.q || null);
	set('sort', s.sort === DEFAULT_SORT ? null : s.sort);
	set('hidden', s.hidden ? '1' : null);
	set('exact', s.exact ? '1' : null);
	set('browse', s.browse ? '1' : null);
	return u.pathname + u.search + u.hash;
}

export type Nav = {
	url: () => URL;
	go: (url: string, push: boolean) => Promise<void>;
};

const kitNav: Nav = {
	url: () => page.url,
	// The URL is the current page's own path with new parameters, so there is no route id to resolve.
	// eslint-disable-next-line svelte/no-navigation-without-resolve
	go: (url, push) => goto(url, { replaceState: !push, keepFocus: true, noScroll: true })
};

export function createSearchState(nav: Nav = kitNav) {
	let text = $state('');
	let ast = $state.raw<Ast | null>(null);
	let parsedQuery = $state<string | null>(null);
	let parseError = $state.raw<ParseError | null>(null);
	let parsing = $state(false);

	const slot = latest();
	let timer: ReturnType<typeof setTimeout> | undefined;
	/** The `q` this state last wrote or adopted; a different one in the URL came from outside. */
	let written: string | undefined;

	const url = () => readUrl(nav.url().searchParams);

	/**
	 * Parses `t` on the server (latest wins). Returns the canonical text, null on an error, or
	 * undefined when a newer run superseded this one, or the draft moved on from `t` (the caller
	 * must then do nothing: the newer draft's own parse owns `ast`).
	 */
	async function parse(t: string): Promise<string | null | undefined> {
		parsing = true;
		try {
			const r = await slot.run((signal) => api.get('/parse', { query: { q: t }, signal }));
			if (t !== text) return undefined;
			ast = r.ast;
			parsedQuery = r.canonical;
			parseError = null;
			parsing = false;
			return r.canonical;
		} catch (e) {
			if (isAbort(e)) return undefined; // superseded: the newer run owns `parsing`
			parsing = false;
			ast = null;
			parseError =
				e instanceof ApiError
					? { code: e.code, message: e.message, start: e.start, end: e.end }
					: { code: 'unknown', message: String(e) };
			return null;
		}
	}

	async function commit(q: string, push: boolean, over: Partial<UrlState> = {}) {
		written = q;
		const target = writeUrl(nav.url(), { ...url(), ...over, q });
		const here = nav.url();
		if (target === here.pathname + here.search + here.hash) return; // no duplicate history entry
		await nav.go(target, push);
	}

	return {
		/** Stop pending work when the query editor leaves; the state can be mounted again. */
		cancelPending() {
			clearTimeout(timer);
			slot.cancel();
			parsing = false;
			written = undefined;
		},
		get text() {
			return text;
		},
		get ast() {
			return ast;
		},
		/** Prevent an old AST (including match-all) being dispatched for a new URL query. */
		get queryReady() {
			return ast !== null && !parsing && !parseError && parsedQuery === url().q;
		},
		get parseError() {
			return parseError;
		},
		get parsing() {
			return parsing;
		},
		get q() {
			return url().q;
		},
		get sort() {
			return url().sort;
		},
		get hidden() {
			return url().hidden;
		},
		get exact() {
			return url().exact;
		},
		get browse() {
			return url().browse === true;
		},

		/** Typing: updates the draft; after the debounce parses it and replaces the URL entry. */
		setText(t: string) {
			text = t;
			clearTimeout(timer);
			// Typing wins over a structured edit or a parse in flight: their answers are for older text.
			slot.cancel();
			timer = setTimeout(async () => {
				const canonical = await parse(t);
				if (canonical === undefined || t !== text) return; // superseded, or typed on meanwhile
				await commit(canonical ?? t, false); // an error keeps the raw text, so a reload shows it
			}, PARSE_DEBOUNCE_MS);
		},

		/** Enter: parses now, normalises the draft to the canonical text and pushes an entry. */
		async submit() {
			clearTimeout(timer);
			const t = text;
			const canonical = await parse(t);
			if (canonical === undefined || t !== text) return;
			if (canonical !== null) text = canonical;
			await commit(canonical ?? t, true);
		},

		/**
		 * A structured edit (a removed chip, the Builder): the server prints it back as text.
		 * `options.hidden` changes the flag in the same history entry (the strip's Clear all).
		 */
		async setAst(next: Ast, options: { hidden?: boolean } = {}) {
			clearTimeout(timer);
			const draft = text;
			parsing = true; // supersedes any parse in flight, so this call owns the flag
			try {
				const r = await slot.run((signal) =>
					// `limit: 1`: only the canonical text is wanted; the page fetches the results.
					api.post('/search', {
						body: {
							ast: next,
							q: null,
							sort: null,
							limit: 1,
							offset: null,
							cursor: null,
							hidden: null,
							exact: null,
							tz: null
						},
						signal
					})
				);
				if (text !== draft) return; // typed meanwhile: the newer draft wins
				ast = next;
				parsedQuery = r.canonical;
				parseError = null;
				parsing = false;
				text = r.canonical;
				await commit(
					r.canonical,
					true,
					options.hidden === undefined ? {} : { hidden: options.hidden }
				);
			} catch (e) {
				if (isAbort(e)) return; // a newer run owns `parsing`
				parsing = false;
				throw e;
			}
		},

		/** Clears the query and the options; other URL parameters are kept. */
		async reset() {
			clearTimeout(timer);
			slot.cancel();
			text = '';
			ast = null;
			parsedQuery = null;
			parseError = null;
			parsing = false;
			await commit('', true, { sort: DEFAULT_SORT, hidden: false, exact: false, browse: false });
		},

		async setSort(sort: string) {
			await commit(url().q, true, { sort: SORT.test(sort) ? sort : DEFAULT_SORT });
		},
		async setHidden(hidden: boolean) {
			await commit(url().q, true, { hidden });
		},
		async setExact(exact: boolean) {
			await commit(url().q, true, { exact });
		},

		/** Call when the URL may have changed (an effect on `page.url`): adopts outside changes. */
		sync() {
			const q = url().q;
			if (q === written) return;
			written = q;
			clearTimeout(timer);
			text = q;
			void parse(q);
		}
	};
}

export const search = createSearchState();
