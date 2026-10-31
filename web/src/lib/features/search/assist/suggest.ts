// `/suggest` for the bar: debounced, latest wins, and a request still in flight is aborted by the
// next keystroke (the server cancels its work when the client disconnects, so a stale one costs
// nothing). A failed request is just no suggestions: typing never waits on this.
import { api, isAbort, latest } from '$lib/api/client';
import type { SuggestItem } from './options';

export const SUGGEST_DEBOUNCE_MS = 120;

export function createSuggester(
	onitems: (items: SuggestItem[]) => void,
	hidden: () => boolean = () => false
) {
	const slot = latest();
	let timer: ReturnType<typeof setTimeout> | undefined;
	return {
		/** `pos` is the caret in characters. */
		request(q: string, pos: number) {
			clearTimeout(timer);
			timer = setTimeout(async () => {
				try {
					const r = await slot.run((signal) =>
						api.get('/suggest', { query: { q, pos, hidden: hidden() || undefined }, signal })
					);
					onitems(r.items);
				} catch (e) {
					if (!isAbort(e)) onitems([]);
				}
			}, SUGGEST_DEBOUNCE_MS);
		},
		cancel() {
			clearTimeout(timer);
			slot.cancel();
		}
	};
}
