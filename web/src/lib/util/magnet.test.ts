import { describe, expect, it } from 'vitest';
import { isMagnet } from './magnet';

describe('isMagnet', () => {
	it('accepts a magnet URI', () => {
		expect(isMagnet('magnet:?xt=urn:btih:abc')).toBe(true);
	});
	it.each(['javascript:alert(1)', 'MAGNET:?xt=1', ' magnet:?xt=1', 'magnet:xt=1', 'http://x', ''])(
		'rejects %j',
		(value) => expect(isMagnet(value)).toBe(false)
	);
	it('rejects non-strings', () => {
		expect(isMagnet(null)).toBe(false);
		expect(isMagnet(undefined)).toBe(false);
		expect(isMagnet(42)).toBe(false);
	});
});
