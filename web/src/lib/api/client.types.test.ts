// Compile-time checks (svelte-check / tsc): the `@ts-expect-error` lines must stay errors.
import { expectTypeOf, it } from 'vitest';
import { api } from './client';
import type { components } from './schema';

it('types the wrapper from the generated paths', () => {
	// Never executed: only the types matter.
	const _types = () => {
		// Optional options.
		void api.get('/settings');
		void api.get('/parse', { query: { q: 'x' } });
		// A required path param.
		void api.get('/torrents/{hash}', { path: { hash: 'ab' } });
		// @ts-expect-error path params are required
		void api.get('/torrents/{hash}');
		// @ts-expect-error path params are required, options can't be empty
		void api.get('/torrents/{hash}', {});
		// A required body.
		void api.post('/login', { body: { password: 'x' } });
		// @ts-expect-error the body is required
		void api.post('/login');
		// @ts-expect-error the body is required
		void api.post('/login', {});
		// @ts-expect-error unknown body field
		void api.post('/login', { body: { nope: 1 } });
		// @ts-expect-error GET takes no body
		void api.get('/parse', { body: {} });
		// @ts-expect-error not a GET path
		void api.get('/login');
		// @ts-expect-error unknown query param
		void api.get('/parse', { query: { zzz: 1 } });
	};
	void _types;

	expectTypeOf<Awaited<ReturnType<() => ReturnType<typeof api.get<'/parse'>>>>>().toEqualTypeOf<
		components['schemas']['ParseResponse']
	>();
	// A 202 response is typed too.
	expectTypeOf<Awaited<ReturnType<typeof api.post<'/filters/preview/jobs'>>>>().toEqualTypeOf<
		components['schemas']['JobStarted']
	>();
});
