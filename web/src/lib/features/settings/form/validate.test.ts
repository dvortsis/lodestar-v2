import { describe, expect, it } from 'vitest';
import { spec } from './testing';
import { isHost, isHttpUrl, isIPv6, isSocketAddr, validate } from './validate';
import { RESET } from './values';

describe('validate', () => {
	const timeout = spec('crawler.timeout_secs', {
		kind: 'integer',
		minimum: 1,
		maximum: 120,
		unit: 'seconds'
	});

	it('checks both bounds, with the unit in the message', () => {
		expect(validate(timeout, 1)).toBeUndefined();
		expect(validate(timeout, 120)).toBeUndefined();
		expect(validate(timeout, 0)).toBe('Must be between 1 and 120 seconds.');
		expect(validate(timeout, 121)).toBe('Must be between 1 and 120 seconds.');
	});

	it('formats bounds through format and names unknown units as sent', () => {
		const port = spec('network.dht_port', { kind: 'integer', minimum: 1, maximum: 65535 });
		expect(validate(port, 70000)).toBe('Must be between 1 and 65,535.');
		const mmap = spec('storage.mmap_mib', { kind: 'integer', minimum: 0, unit: 'mebibytes' });
		expect(validate(mmap, -1)).toBe('Must be at least 0 MiB.');
		const odd = spec('s.f', { kind: 'number', maximum: 2, unit: 'furlongs' });
		expect(validate(odd, 3)).toBe('Must be at most 2 furlongs.');
		expect(validate(spec('s.f', { kind: 'number', minimum: 1 }), 0)).toBe('Must be at least 1.');
		expect(validate(spec('s.f', { kind: 'number', maximum: 1 }), 2)).toBe('Must be at most 1.');
	});

	it('checks integers and numbers', () => {
		expect(validate(timeout, 1.5)).toBe('Enter a whole number.');
		expect(validate(timeout, '12')).toBe('Enter a number.');
		expect(validate(timeout, NaN)).toBe('Enter a number.');
		expect(validate(timeout, Infinity)).toBe('Enter a number.');
		expect(validate(spec('s.f', { kind: 'number' }), 1.5)).toBeUndefined();
	});

	it('checks enums and types', () => {
		const e = spec('s.f', { kind: 'enum', enum: ['low', 'balanced'] });
		expect(validate(e, 'low')).toBeUndefined();
		expect(validate(e, 'high')).toBe('Choose one of the options.');
		expect(validate(spec('s.f', { kind: 'boolean' }), 'true')).toMatch(/right kind/);
		expect(validate(spec('s.f', { kind: 'string-list' }), ['a', 1])).toMatch(/right kind/);
		expect(validate(spec('s.f', { kind: 'string-list' }), ['a'])).toBeUndefined();
		expect(validate(spec('s.f', { kind: 'map' }), [])).toMatch(/right kind/);
		expect(validate(spec('s.f', { kind: 'map' }), { a: 1 })).toBeUndefined();
		expect(validate(spec('s.f', { kind: 'string' }), 1)).toMatch(/right kind/);
		expect(validate(spec('s.f', { kind: 'unknown' }), Symbol)).toBeUndefined();
	});

	it('never validates a reset or a secret', () => {
		expect(validate(timeout, RESET)).toBeUndefined();
		expect(validate(spec('s.f', { writeOnly: true, format: 'url' }), 'x')).toBeUndefined();
	});

	it('checks formats, and accepts an automatic empty string', () => {
		const bind = spec('network.http_bind', { format: 'socket-addr' });
		expect(validate(bind, '0.0.0.0:8090')).toBeUndefined();
		expect(validate(bind, 'localhost:8090')).toMatch(/address and a port/);
		expect(validate(spec('s.f', { format: 'url' }), 'ftp://x')).toMatch(/http/);
		expect(validate(spec('s.f', { format: 'host' }), 'bad host')).toMatch(/host name/);
		expect(validate(spec('s.f', { format: 'log-filter' }), '!!!')).toBeUndefined();
		expect(validate(spec('s.f', { auto: '' }), '')).toBeUndefined();
	});
});

describe('socket-addr', () => {
	it.each([
		'0.0.0.0:8090',
		'127.0.0.1:1',
		'192.168.1.5:65535',
		'[::]:8090',
		'[::1]:80',
		'[fe80::1]:6881',
		'[2001:db8::8a2e:370:7334]:443',
		'[1:2:3:4:5:6:7:8]:1',
		'[::ffff:192.168.1.5]:8090'
	])('accepts %s', (s) => expect(isSocketAddr(s)).toBe(true));

	it.each([
		'',
		'8090',
		':8090',
		'0.0.0.0',
		'0.0.0.0:',
		'0.0.0.0:0',
		'0.0.0.0:65536',
		'0.0.0.0:99999',
		'0.0.0.0:-1',
		'256.0.0.1:80',
		'1.2.3:80',
		'01.2.3.4:80',
		'localhost:80',
		'::1:80',
		'[::1]',
		'[::1]:0',
		'[1:2:3:4:5:6:7:8:9]:1',
		'[1::2::3]:1',
		'[fe80::1%eth0]:80',
		'[12345::1]:80',
		' 0.0.0.0:80'
	])('refuses %j', (s) => expect(isSocketAddr(s)).toBe(false));

	it('reads IPv6 forms', () => {
		expect(isIPv6('::')).toBe(true);
		expect(isIPv6('1::')).toBe(true);
		expect(isIPv6('1:2:3:4:5:6:7::')).toBe(true);
		expect(isIPv6('1:2:3:4:5:6::1.2.3.4')).toBe(false);
		expect(isIPv6('1:2:3:4:5::1.2.3.4')).toBe(true);
		expect(isIPv6('1:2:3:4:5:6:7')).toBe(false);
	});
});

describe('host and url', () => {
	it('accepts names and addresses', () => {
		for (const h of ['nas.local', 'example.com.', '192.168.1.5', '::1', '[::1]', 'a-b.c']) {
			expect(isHost(h), h).toBe(true);
		}
		for (const h of ['', '-a.com', 'a..b', 'a b', 'a_b.com', '[nope]', 'x'.repeat(64)]) {
			expect(isHost(h), h).toBe(false);
		}
	});

	it('accepts http and https only', () => {
		expect(isHttpUrl('http://192.168.1.5:8080/')).toBe(true);
		expect(isHttpUrl('https://example.com')).toBe(true);
		expect(isHttpUrl('ftp://example.com')).toBe(false);
		expect(isHttpUrl('javascript:alert(1)')).toBe(false);
		expect(isHttpUrl('example.com')).toBe(false);
	});
});
