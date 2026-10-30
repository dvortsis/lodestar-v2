// Shared by the search results and facet specs: typed bodies, a parser stand-in (words, `-x`,
// tags, flags, `ext:`, ranges and `discovered:`; an unclosed quote is a syntax error) that prints
// ASTs back, and the requests opening a row makes.
import { expect as base, type Page, type Request } from '@playwright/test';
import type { components } from '../src/lib/api/schema';
import { detail, hit, siblings } from './detail.fixtures';
import type { OpenApiMock } from './mock/openapi-mock';

/** `expect` with the 10 s deadline the slowest CI runner needs (CLAUDE.md: no fixed sleeps). */
export const expect = base.configure({ timeout: 10_000 });

export type Ast = components['schemas']['AstNode'];
export type Hit = components['schemas']['SearchHit'];
export type Response = components['schemas']['SearchResponse'];

const TAGS = ['type', 'res', 'hdr', 'codec', 'audio', 'lang', 'group', 'has', 'adult'];
const FLAGS = ['private', 'v2', 'ambiguous', 'imported', 'backfill'];
const NUMS = ['size', 'seeders', 'trackers'];
type TagKey = Extract<Ast, { op: 'tag' }>['key'];
type NumField = Extract<Ast, { op: 'range' }>['field'];

/** One term of the stand-in language: words, `-x`, tags, flags, `ext:`, ranges, `discovered:`. */
function term(w: string): Ast {
	if (w.startsWith('-') && w.length > 1) return { op: 'not', of: term(w.slice(1)) };
	const at = w.indexOf(':');
	const key = at > 0 ? w.slice(0, at) : '';
	const v = w.slice(at + 1);
	if (key === 'ext') return { op: 'ext', any: v.split(',') };
	if (TAGS.includes(key)) return { op: 'tag', key: key as TagKey, any: v.split(',') };
	if (FLAGS.includes(key)) return { op: 'flag', name: key, value: v === 'yes' };
	if (NUMS.includes(key)) {
		const field = key as NumField;
		if (v.startsWith('>=')) return { op: 'range', field, lo: Number(v.slice(2)), hi: null };
		if (v.startsWith('<=')) return { op: 'range', field, lo: null, hi: Number(v.slice(2)) };
		const [lo, hi] = v.split('..');
		return { op: 'range', field, lo: Number(lo), hi: Number(hi) };
	}
	if (key === 'discovered') {
		if (v.startsWith('<'))
			return { op: 'date', field: 'discovered', from: { ago: Number(v.slice(1)) }, to: null };
		const [from, to] = v.split('..');
		return {
			op: 'date',
			field: 'discovered',
			from: from ? { at: Number(from) } : null,
			to: to ? { at: Number(to) } : null
		};
	}
	return { op: 'word', field: 'any', word: w };
}

export const toAst = (q: string): Ast => {
	const of = q.split(/\s+/).filter(Boolean).map(term);
	return of.length === 0 ? { op: 'all' } : of.length === 1 ? of[0]! : { op: 'and', of };
};

function printTerm(n: Ast): string {
	switch (n.op) {
		case 'word':
			return n.word;
		case 'not':
			return `-${printTerm(n.of)}`;
		case 'ext':
			return `ext:${n.any.join(',')}`;
		case 'tag':
			return `${n.key}:${n.any.join(',')}`;
		case 'flag':
			return `${n.name}:${n.value ? 'yes' : 'no'}`;
		case 'range':
			return n.hi === null
				? `${n.field}:>=${n.lo}`
				: n.lo === null
					? `${n.field}:<=${n.hi}`
					: `${n.field}:${n.lo}..${n.hi}`;
		case 'date': {
			const end = (w: typeof n.from) => (w === null ? '' : 'at' in w ? String(w.at) : '');
			return n.from && 'ago' in n.from && n.to === null
				? `discovered:<${n.from.ago}`
				: `discovered:${end(n.from)}..${end(n.to)}`;
		}
		default:
			return '';
	}
}

/** The stand-in's canonical text, as the server prints an AST back. */
export const print = (ast: Ast): string =>
	(ast.op === 'and' ? ast.of : ast.op === 'all' ? [] : [ast]).map(printTerm).join(' ');

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
	sampling_blocked: null,
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
