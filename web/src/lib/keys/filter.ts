// Command palette matching: every word of the query must appear; a title that starts with the
// word ranks first, then a word start in the title, then anywhere in the title, then the keywords
// and section. Ties keep registration order, so the list doesn't jump while typing.
export interface Searchable {
	title: string;
	keywords?: readonly string[];
	section?: string;
}

function wordStarts(text: string, word: string): boolean {
	let from = text.indexOf(word);
	while (from !== -1) {
		if (from === 0 || /[\s\-_/.:·]/.test(text[from - 1]!)) return true;
		from = text.indexOf(word, from + 1);
	}
	return false;
}

function score(item: Searchable, word: string): number {
	const title = item.title.toLowerCase();
	if (title.startsWith(word)) return 0;
	if (wordStarts(title, word)) return 1;
	if (title.includes(word)) return 2;
	const extra = [...(item.keywords ?? []), item.section ?? ''].join(' ').toLowerCase();
	return extra.includes(word) ? 3 : -1;
}

export function filterCommands<T extends Searchable>(items: readonly T[], query: string): T[] {
	const words = query.toLowerCase().split(/\s+/).filter(Boolean);
	if (!words.length) return [...items];
	const ranked: { item: T; total: number; index: number }[] = [];
	items.forEach((item, index) => {
		let total = 0;
		for (const word of words) {
			const s = score(item, word);
			if (s < 0) return;
			total += s;
		}
		ranked.push({ item, total, index });
	});
	return ranked.sort((a, b) => a.total - b.total || a.index - b.index).map((r) => r.item);
}
