import { describe, expect, it } from 'vitest';
import { acceptUrl, healthUrl, networkUrl } from './urls';

describe('acceptUrl', () => {
	it.each([
		['http://192.168.1.5:9090', 'http://192.168.1.5:9090/'],
		['https://lodestar.example.com', 'https://lodestar.example.com/'],
		['http://[::1]:8090/', 'http://[::1]:8090/'],
		['http://[2001:db8::7]:9000', 'http://[2001:db8::7]:9000/'],
		['HTTP://NAS.LOCAL:8090', 'http://nas.local:8090/'],
		['http://nas.local:65535', 'http://nas.local:65535/']
	])('accepts %s', (given, href) => {
		expect(acceptUrl(given)?.href).toBe(href);
	});

	it.each([
		'javascript:alert(1)',
		'data:text/html,<script>alert(1)</script>',
		'file:///etc/passwd',
		'ftp://nas.local',
		'blob:http://nas.local/uuid',
		'//nas.local:8090',
		'nas.local:8090',
		'192.168.1.5:9090',
		'http://',
		'http://nas.local:99999',
		'http://exa mple.com',
		'',
		'   '
	])('rejects %j', (given) => {
		expect(acceptUrl(given)).toBeNull();
	});

	it('keeps credentials out of nothing it should not: the address is taken as given', () => {
		// The server names the address; credentials in it are the server's, and are not followed
		// differently. (Browsers drop them from the request.)
		expect(acceptUrl('http://user:pw@nas.local:8090')?.hostname).toBe('nas.local');
	});
});

describe('healthUrl and networkUrl', () => {
	it('builds the health check and the landing page on the new origin', () => {
		expect(healthUrl('http://192.168.1.5:9090/ui?x=1#y')?.href).toBe(
			'http://192.168.1.5:9090/healthz'
		);
		expect(networkUrl('http://[::1]:8090')?.href).toBe('http://[::1]:8090/settings/network');
	});

	it('returns null for what is not followed', () => {
		expect(healthUrl('javascript:alert(1)')).toBeNull();
		expect(networkUrl('not a url')).toBeNull();
	});
});
