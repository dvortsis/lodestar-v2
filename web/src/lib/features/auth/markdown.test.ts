import { describe, expect, it } from 'vitest';
import { parseBlocks, parseInline } from './markdown';

describe('parseInline', () => {
	it('reads bold, italics, code and nesting', () => {
		expect(parseInline('a **b `c`** *d* `e`')).toEqual([
			{ t: 'text', v: 'a ' },
			{
				t: 'strong',
				c: [
					{ t: 'text', v: 'b ' },
					{ t: 'code', v: 'c' }
				]
			},
			{ t: 'text', v: ' ' },
			{ t: 'em', c: [{ t: 'text', v: 'd' }] },
			{ t: 'text', v: ' ' },
			{ t: 'code', v: 'e' }
		]);
	});

	it('links bare https URLs, leaving the sentence punctuation out', () => {
		expect(parseInline('at https://github.com/dvortsis/lodestar-v2.')).toEqual([
			{ t: 'text', v: 'at ' },
			{
				t: 'link',
				href: 'https://github.com/dvortsis/lodestar-v2',
				v: 'https://github.com/dvortsis/lodestar-v2'
			},
			{ t: 'text', v: '.' }
		]);
		expect(parseInline('(https://report.cybertip.org) in')).toEqual([
			{ t: 'text', v: '(' },
			{ t: 'link', href: 'https://report.cybertip.org', v: 'https://report.cybertip.org' },
			{ t: 'text', v: ') in' }
		]);
	});

	it('keeps unmatched markers, other schemes and HTML as text', () => {
		expect(parseInline('2 * 3 and **x')).toEqual([{ t: 'text', v: '2 * 3 and **x' }]);
		expect(parseInline('javascript:alert(1) <b>x</b> http://a')).toEqual([
			{ t: 'text', v: 'javascript:alert(1) <b>x</b> http://a' }
		]);
		expect(parseInline('https://')).toEqual([{ t: 'text', v: 'https://' }]);
	});
});

describe('parseBlocks', () => {
	it('reads headings, paragraphs and lists', () => {
		const text = '# Title\n\n**V 2**\n\nOne\ntwo.\n## 1. Part\n- a\n- b\n  wrapped\nAfter.\n';
		expect(parseBlocks(text)).toEqual([
			{ t: 'heading', level: 1, c: [{ t: 'text', v: 'Title' }] },
			{ t: 'p', c: [{ t: 'strong', c: [{ t: 'text', v: 'V 2' }] }] },
			{ t: 'p', c: [{ t: 'text', v: 'One two.' }] },
			{ t: 'heading', level: 2, c: [{ t: 'text', v: '1. Part' }] },
			{
				t: 'ul',
				items: [[{ t: 'text', v: 'a' }], [{ t: 'text', v: 'b wrapped' }]]
			},
			{ t: 'p', c: [{ t: 'text', v: 'After.' }] }
		]);
	});

	it('takes Windows line ends', () => {
		expect(parseBlocks('a\r\n\r\nb')).toEqual([
			{ t: 'p', c: [{ t: 'text', v: 'a' }] },
			{ t: 'p', c: [{ t: 'text', v: 'b' }] }
		]);
	});
});
