import { describe, expect, it } from 'vitest';
import { loginUrl, safeNext } from './next';

const O = 'http://lodestar.local:8090';

describe('safeNext', () => {
	it.each([
		['/search?q=ubuntu&sort=size', '/search?q=ubuntu&sort=size'],
		['/t/0123?x=1#files', '/t/0123?x=1#files'],
		['/settings/general', '/settings/general'],
		['/', '/'],
		['/a/../search', '/search']
	])('keeps the same-origin path %s', (raw, want) => {
		expect(safeNext(raw, O)).toBe(want);
	});

	it.each([
		null,
		'',
		'search',
		'//evil.example',
		'//evil.example/search',
		'/\\evil.example',
		'\\\\evil.example',
		'/\t/evil.example',
		'/\n/evil.example',
		'https://evil.example/',
		'http://lodestar.local:8091/',
		'javascript:alert(1)',
		' /search',
		'%2F%2Fevil.example',
		'/login',
		'/login?next=/search',
		'/setup',
		'/setup/',
		'/api/v1/logout',
		'/' + 'a'.repeat(3000)
	])('refuses %j', (raw) => {
		expect(safeNext(raw, O)).toBe('/');
	});

	it('keeps an encoded double slash as a path on this origin', () => {
		expect(safeNext('/%2F%2Fevil.example', O)).toBe('/%2F%2Fevil.example');
	});
});

describe('loginUrl', () => {
	it('carries next, encoded', () => {
		expect(loginUrl('/search?q=a&b=c')).toBe('/login?next=%2Fsearch%3Fq%3Da%26b%3Dc');
		expect(loginUrl('/')).toBe('/login');
		expect(loginUrl('')).toBe('/login');
	});

	it('round-trips through safeNext', () => {
		const next = '/search?q=ubuntu 24.04&sort=size';
		const url = new URL(loginUrl(next), O);
		expect(safeNext(url.searchParams.get('next'), O)).toBe('/search?q=ubuntu%2024.04&sort=size');
	});
});
