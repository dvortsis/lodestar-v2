import { describe, expect, it } from 'vitest';
import { trackerHost } from './tracker';

describe('trackerHost', () => {
	it('keeps only the host', () => {
		expect(trackerHost('https://tracker.example.org:8443/PASSKEY123/announce?pk=SECRET')).toBe(
			'tracker.example.org'
		);
		expect(trackerHost('udp://open.example.net:6969/announce')).toBe('open.example.net');
		expect(trackerHost('https://user:pw@t.example.com/x')).toBe('t.example.com');
	});
	it('returns null for something that is not a URL', () => {
		expect(trackerHost('not a url')).toBeNull();
		expect(trackerHost('')).toBeNull();
	});
});
