// Shared by the search results specs (routes and keyboard): typed bodies, a parser stand-in that
// knows words and `ext:` (an unclosed quote is a syntax error), and the requests opening a row makes.
import { expect as base, type Page, type Request } from '@playwright/test';
import type { components } from '../src/lib/api/schema';
import { detail, hit, siblings } from './detail.fixtures';
import type { OpenApiMock } from './mock/openapi-mock';

/** `expect` with the 10 s deadline the slowest CI runner needs (CLAUDE.md: no fixed sleeps). */
export const expect = base.configure({ timeout: 10_000 });

export type Ast = components['schemas']['AstNode'];
export type Hit = components['schemas']['SearchHit'];
export type Response = components['schemas']['SearchResponse'];

export const toAst = (q: string): Ast => {
	const of = q
		.split(/\s+/)
		.filter(Boolean)
		.map((w): Ast =>
			w.startsWith('ext:')
				? { op: 'ext', any: [w.slice(4)] }
				: { op: 'word', field: 'any', word: w }
		);
	return of.length === 0 ? { op: 'all' } : of.length === 1 ? of[0]! : { op: 'and', of };
};

/** The stand-in's canonical text, as the server prints an AST back. */
export const print = (ast: Ast): string =>
	(ast.op === 'and' ? ast.of : ast.op === 'all' ? [] : [ast])
		.map((n) => (n.op === 'word' ? n.word : n.op === 'ext' ? `ext:${n.any[0]}` : ''))
		.join(' ');

export const hashOf = (n: number) => n.toString(16).padStart(40, 'a');
export const hits = (n: number, from = 0): Hit[] =>
	Array.from({ length: n }, (_, i) =>
		hit({
			id: from + i + 1,
			hash: hashOf(from + i + 1),
			title: `Lantern Keeper ${from + i + 1}`,
			name: `Lantern.Keeper.${from + i + 1}.1080p`
		})
	);
export const body = (h: Hit[], over: Partial<Response> = {}): Response => ({
	canonical: '',
	hits: h,
	limit: 50,
	next_cursor: null,
	notices: [],
	offset: 0,
	stemmed: false,
	took_ms: 1,
	total: h.length,
	total_approx: false,
	...over
});

export const figures = (searchable: number) => ({
	data_bytes: null,
	disk_free_bytes: null,
	fetched_per_hour: null,
	harvested_per_hour: null,
	index_lag_s: null,
	paused: false,
	queue_depth: 0,
	searchable_torrents: searchable,
	stored_torrents: searchable,
	updated_at: 1_700_000_000,
	uptime_s: 60
});

export type Answer = Response | { status: number; body: unknown };

type Sent = { ast: Ast; limit: number; cursor: string | null };

/** The results requests (limit 50) the page has sent so far. */
export const resultsCalls = (mock: OpenApiMock) =>
	mock
		.calls('searchPost')
		.map((r) => r.postDataJSON() as Sent)
		.filter((b) => b.limit !== 1);

/** The parser stand-in and the empty state's requests; `answer` decides each results request. */
export function server(mock: OpenApiMock, answer: (req: Sent) => Answer) {
	mock.on('parseQuery', (request: Request) => {
		const q = new URL(request.url()).searchParams.get('q') ?? '';
		const quote = q.indexOf('"');
		if (quote >= 0 && q.split('"').length % 2 === 0)
			return {
				status: 400,
				body: {
					error: {
						code: 'query_syntax',
						message: 'unclosed quote',
						field: 'q',
						start: Buffer.byteLength(q.slice(0, quote)),
						end: Buffer.byteLength(q)
					}
				}
			};
		return { ast: toAst(q), canonical: q.trim().replace(/\s+/g, ' ') };
	});
	mock.on('listSavedSearches', []);
	mock.on('searchPost', (request: Request) => {
		const sent = request.postDataJSON() as Sent;
		// W5's own request (limit 1, a removed chip) wants only the canonical text.
		if (sent.limit === 1) return body([], { canonical: print(sent.ast) });
		return answer(sent);
	});
	// Opening a row: the drawer's requests.
	mock.on('getTorrent', (request: Request) => {
		const h = new URL(request.url()).pathname.split('/').pop()!;
		return { status: 200, body: detail({ hash: h, magnet: `magnet:?xt=urn:btih:${h}` }) };
	});
	mock.on('getTorrentSiblings', siblings());
	mock.on('listTorrentFiles', { files: [], offset: 3, total: 3 });
}

export const row = (page: Page, n: number) => page.getByTestId(`result-${hashOf(n)}`);
export const rowOf = (page: Page, n: number) =>
	page.getByRole('row').filter({ has: page.getByTestId(`result-${hashOf(n)}`) });
export const grid = (page: Page) => page.getByRole('grid', { name: 'Matching torrents' });

/**
 * Records what the page copies, in the page: the system clipboard is shared by parallel workers,
 * so reading it back can see another test's copy. Call before `go`; read with `copied`.
 */
export async function recordClipboard(page: Page) {
	await page.addInitScript(() => {
		const w = window as unknown as { __copied: string[] };
		w.__copied = [];
		Object.defineProperty(navigator, 'clipboard', {
			value: {
				writeText: async (text: string) => {
					w.__copied.push(text);
				}
			}
		});
	});
}
export const copied = (page: Page) =>
	page.evaluate(() => (window as unknown as { __copied: string[] }).__copied.at(-1) ?? null);
