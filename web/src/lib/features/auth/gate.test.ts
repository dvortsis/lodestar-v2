import { describe, expect, it } from 'vitest';
import { redirectFor } from './gate';

const status = (setup_required: boolean, logged_in: boolean) => ({ setup_required, logged_in });

describe('redirectFor', () => {
	it('sign-in page: setup first, signed-in visitors on to next, otherwise the form', () => {
		expect(redirectFor('login', status(true, false), '/x')).toBe('/setup');
		expect(redirectFor('login', status(false, true), '/x')).toBe('/x');
		expect(redirectFor('login', status(false, false), '/x')).toBeNull();
	});

	it('setup page: once set up, always to sign in', () => {
		expect(redirectFor('setup', status(true, false), '/')).toBeNull();
		expect(redirectFor('setup', status(false, false), '/')).toBe('/login');
		expect(redirectFor('setup', status(false, true), '/')).toBe('/login');
	});

	it('shows the form when the check failed', () => {
		expect(redirectFor('login', null, '/x')).toBeNull();
		expect(redirectFor('setup', null, '/')).toBeNull();
	});
});
