// The Legal Notice's Markdown, as data the component renders node by node: never as HTML, so
// nothing in the text can become markup. Only what the notice uses (the server's tests keep it to
// that): `#`/`##` headings, paragraphs, `- ` lists, **bold**, *italics*, `code` and bare
// https:// links. Anything else shows as its text.

export type Inline =
	| { t: 'text'; v: string }
	| { t: 'strong'; c: Inline[] }
	| { t: 'em'; c: Inline[] }
	| { t: 'code'; v: string }
	| { t: 'link'; href: string; v: string };

export type Block =
	| { t: 'heading'; level: 1 | 2; c: Inline[] }
	| { t: 'p'; c: Inline[] }
	| { t: 'ul'; items: Inline[][] };

/** Splits the text into headings, paragraphs and lists. */
export function parseBlocks(text: string): Block[] {
	const out: Block[] = [];
	let para: string[] = [];
	let items: string[] = [];
	const flush = () => {
		if (para.length) out.push({ t: 'p', c: parseInline(para.join(' ')) });
		if (items.length) out.push({ t: 'ul', items: items.map(parseInline) });
		para = [];
		items = [];
	};
	for (const raw of text.replace(/\r\n?/g, '\n').split('\n')) {
		const line = raw.trim();
		const heading = /^(#{1,2}) (.*)$/.exec(line);
		const [, hashes = '', title = ''] = heading ?? [];
		if (!line) {
			flush();
		} else if (heading) {
			flush();
			out.push({
				t: 'heading',
				level: hashes.length as 1 | 2,
				c: parseInline(title)
			});
		} else if (line.startsWith('- ')) {
			if (para.length) flush();
			items.push(line.slice(2));
		} else if (items.length && raw.startsWith(' ')) {
			// A wrapped list item.
			items[items.length - 1] += ' ' + line;
		} else {
			if (items.length) flush();
			para.push(line);
		}
	}
	flush();
	return out;
}

const URL_START = 'https://';
/** Punctuation that ends a sentence after a bare link, not part of it. */
const TRAILING = /[.,;:!?]+$/;

/** Bold, italics, code and bare links; an unmatched marker stays as text. */
export function parseInline(s: string): Inline[] {
	const out: Inline[] = [];
	let text = '';
	const push = (node: Inline) => {
		if (text) out.push({ t: 'text', v: text });
		text = '';
		out.push(node);
	};
	let i = 0;
	while (i < s.length) {
		if (s.startsWith('**', i)) {
			const end = s.indexOf('**', i + 2);
			if (end > i + 2) {
				push({ t: 'strong', c: parseInline(s.slice(i + 2, end)) });
				i = end + 2;
				continue;
			}
		} else if (s[i] === '*') {
			const end = s.indexOf('*', i + 1);
			if (end > i + 1 && s[end + 1] !== '*') {
				push({ t: 'em', c: parseInline(s.slice(i + 1, end)) });
				i = end + 1;
				continue;
			}
		} else if (s[i] === '`') {
			const end = s.indexOf('`', i + 1);
			if (end > i + 1) {
				push({ t: 'code', v: s.slice(i + 1, end) });
				i = end + 1;
				continue;
			}
		} else if (s.startsWith(URL_START, i)) {
			let end = i;
			while (end < s.length && !/[\s<>"`*]/.test(s.charAt(end))) end++;
			let href = s.slice(i, end);
			href = href.replace(TRAILING, '');
			// "(https://example.org)": the closing bracket is the sentence's.
			if (href.endsWith(')') && !href.includes('(')) href = href.slice(0, -1);
			if (href.length > URL_START.length) {
				push({ t: 'link', href, v: href });
				i += href.length;
				continue;
			}
		}
		text += s.charAt(i);
		i++;
	}
	if (text) out.push({ t: 'text', v: text });
	return out;
}
