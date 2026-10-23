import { describe, expect, it } from 'vitest';
import { configToml, envLines, envValue, tomlDottedKey, tomlString, tomlValue } from './toml';

describe('tomlString', () => {
	it.each([
		['plain', '"plain"'],
		['say "hi"', '"say \\"hi\\""'],
		['C:\\data\\lodestar', '"C:\\\\data\\\\lodestar"'],
		['line one\nline two', '"line one\\nline two"'],
		['tab\there\r\n', '"tab\\there\\r\\n"'],
		['\b\f', '"\\b\\f"'],
		['bell\u0007 and nul\u0000 and del\u007f', '"bell\\u0007 and nul\\u0000 and del\\u007F"'],
		['Zürich 日本語 🧭', '"Zürich 日本語 🧭"'],
		['\ud800 alone', '"\\uFFFD alone"'],
		['alone \udc00', '"alone \\uFFFD"'],
		['', '""'],
		['# not a comment', '"# not a comment"'],
		["it's", '"it\'s"']
	])('writes %j as %s', (given, written) => {
		expect(tomlString(given)).toBe(written);
	});

	it('escapes backslash before quote, so a trailing backslash cannot close the string', () => {
		expect(tomlString('a\\')).toBe('"a\\\\"');
		expect(tomlString('\\"')).toBe('"\\\\\\""');
	});
});

describe('tomlValue', () => {
	it('writes every scalar', () => {
		expect([true, false, 0, 7000, -3, 1.5, -0, 1e21].map(tomlValue)).toEqual([
			'true',
			'false',
			'0',
			'7000',
			'-3',
			'1.5',
			'-0',
			'1e+21'
		]);
		expect([NaN, Infinity, -Infinity].map(tomlValue)).toEqual(['nan', 'inf', '-inf']);
		expect(tomlValue(10n)).toBe('10');
	});

	it('writes lists and maps inline, quoting keys that are not bare', () => {
		expect(tomlValue(['a', 'b"c', 3])).toBe('["a", "b\\"c", 3]');
		expect(tomlValue([])).toBe('[]');
		expect(tomlValue({ dht: 'debug', 'my key': 'x' })).toBe('{ dht = "debug", "my key" = "x" }');
		expect(tomlValue({})).toBe('{}');
		expect(tomlValue({ a: { b: [1, { c: true }] } })).toBe('{ a = { b = [1, { c = true }] } }');
	});

	it('has no form for null, undefined or anything holding one', () => {
		expect(tomlValue(null)).toBeUndefined();
		expect(tomlValue(undefined)).toBeUndefined();
		expect(tomlValue([1, null])).toBeUndefined();
		expect(tomlValue({ a: null })).toBeUndefined();
		expect(tomlValue(() => 1)).toBeUndefined();
	});
});

describe('keys', () => {
	it('keeps dots as separators and quotes parts that need it', () => {
		expect(tomlDottedKey('dht.intensity')).toBe('dht.intensity');
		expect(tomlDottedKey('http.secure_cookies')).toBe('http.secure_cookies');
		expect(tomlDottedKey('a b.c')).toBe('"a b".c');
		expect(tomlDottedKey('é.x')).toBe('"é".x');
	});
});

describe('configToml and envLines', () => {
	const entries = [
		{ config: 'dht.intensity', env: 'LODESTAR_INTENSITY', value: 'low' },
		{ config: 'fetch.timeout_secs', value: 30 },
		{ config: 'log', env: 'LODESTAR_LOG', value: { dht: 'debug' } },
		{ value: false },
		{ config: 'dht.bind', env: 'LODESTAR_DHT_PORT', value: null },
		{ config: 'store.path', env: 'LODESTAR_DATA_DIR', value: 'D:\\Lodestar "data"\nnext' }
	];

	it('writes one line per value with its variable as a comment, skipping what has no key or form', () => {
		expect(configToml(entries)).toBe(
			[
				'dht.intensity = "low"  # LODESTAR_INTENSITY',
				'fetch.timeout_secs = 30',
				'log = { dht = "debug" }  # LODESTAR_LOG',
				'store.path = "D:\\\\Lodestar \\"data\\"\\nnext"  # LODESTAR_DATA_DIR'
			].join('\n')
		);
	});

	it('writes LODESTAR_ lines for scalars only, quoting what a .env file would split', () => {
		expect(envLines(entries)).toBe(
			['LODESTAR_INTENSITY=low', 'LODESTAR_DATA_DIR="D:\\\\Lodestar \\"data\\"\\nnext"'].join('\n')
		);
		expect(envValue(true)).toBe('true');
		expect(envValue(7000)).toBe('7000');
		expect(envValue('0.0.0.0:8090')).toBe('0.0.0.0:8090');
		expect(envValue('')).toBe('""');
		expect(envValue('a b')).toBe('"a b"');
		expect(envValue('$HOME')).toBe('"\\$HOME"');
		expect(envValue(['a'])).toBeUndefined();
		expect(envValue({ a: 1 })).toBeUndefined();
		expect(envValue(null)).toBeUndefined();
		expect(envValue(NaN)).toBeUndefined();
	});

	it('is empty for nothing', () => {
		expect(configToml([])).toBe('');
		expect(envLines([])).toBe('');
	});
});
