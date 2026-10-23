// Pages the rest of a big file list: `GET /torrents/{hash}/files?offset&limit` after the first 500
// that detail carries, at most two requests in flight, delivered in order so the list stays in
// torrent order. Whatever was delivered before a failure stays; call again from `loaded`.
export const PAGE = 1000;
export const LANES = 2;
/** Above this many files the tree is built in a Web Worker (§10.1). */
export const WORKER_THRESHOLD = 2000;

export interface LoadOptions<F> {
	/** Files already held; paging starts here. */
	loaded: number;
	total: number;
	fetchPage: (offset: number, limit: number, signal: AbortSignal) => Promise<F[]>;
	/** Called with each page, in order. */
	onPage: (files: F[]) => void;
	signal: AbortSignal;
}

export async function loadRest<F>(options: LoadOptions<F>): Promise<void> {
	const { loaded, total, fetchPage, onPage, signal } = options;
	const offsets: number[] = [];
	for (let o = loaded; o < total; o += PAGE) offsets.push(o);
	const done = new Map<number, F[]>();
	let next = 0;
	let deliver = 0;
	let failure: unknown;
	let failed = false;

	const lane = async () => {
		while (!failed && !signal.aborted) {
			const i = next++;
			if (i >= offsets.length) return;
			try {
				const files = await fetchPage(offsets[i] as number, PAGE, signal);
				if (signal.aborted) return;
				done.set(i, files);
				for (let page = done.get(deliver); page; page = done.get(deliver)) {
					done.delete(deliver++);
					onPage(page);
				}
			} catch (e) {
				failed = true;
				failure = e;
			}
		}
	};
	await Promise.all(Array.from({ length: Math.min(LANES, offsets.length) }, lane));
	if (failed) throw failure;
}
