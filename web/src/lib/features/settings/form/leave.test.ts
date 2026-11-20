import { describe, expect, it } from 'vitest';
import { leaveDecision, type LeaveInput } from './leave';

const at = (p: string) => new URL(p, 'http://lodestar.test');
const base: LeaveInput = {
	dirty: true,
	type: 'link',
	from: at('/settings/crawler'),
	to: at('/settings/network'),
	bypass: false
};

describe('leaveDecision', () => {
	it('blocks leaving the screen with unsaved changes', () => {
		expect(leaveDecision(base)).toBe('prompt');
		expect(leaveDecision({ ...base, type: 'popstate' })).toBe('prompt'); // Back
		expect(leaveDecision({ ...base, type: 'goto', to: at('/search') })).toBe('prompt');
	});

	it('lets the browser ask when the tab closes or reloads', () => {
		expect(leaveDecision({ ...base, type: 'leave', to: null })).toBe('browser');
	});

	it('allows a hash or query change on the same path', () => {
		expect(leaveDecision({ ...base, to: at('/settings/crawler#crawler.intensity') })).toBe('allow');
		expect(leaveDecision({ ...base, to: at('/settings/crawler?changed=1') })).toBe('allow');
	});

	it('allows anything with nothing unsaved', () => {
		expect(leaveDecision({ ...base, dirty: false })).toBe('allow');
		expect(leaveDecision({ ...base, dirty: false, type: 'leave' })).toBe('allow');
	});

	it('allows its own navigation after Save or Discard', () => {
		expect(leaveDecision({ ...base, bypass: true })).toBe('allow');
	});

	it('treats another origin as leaving', () => {
		expect(leaveDecision({ ...base, to: new URL('http://other.test/settings/crawler') })).toBe(
			'prompt'
		);
	});

	it('answers signin when the session ended and the app goes to sign in', () => {
		const to = at('/login?next=%2Fsettings%2Fcrawler');
		expect(leaveDecision({ ...base, to, signIn: 'expired' })).toBe('signin');
		expect(leaveDecision({ ...base, to, signIn: 'notice' })).toBe('signin');
		// Not without a pending sign-in (a plain link to /login asks as usual).
		expect(leaveDecision({ ...base, to })).toBe('prompt');
		// Only to sign in, and only with something unsaved.
		expect(leaveDecision({ ...base, to: at('/search'), signIn: 'expired' })).toBe('prompt');
		expect(leaveDecision({ ...base, to, signIn: 'expired', dirty: false })).toBe('allow');
		expect(
			leaveDecision({ ...base, to: new URL('http://other.test/login'), signIn: 'expired' })
		).toBe('prompt');
	});

	it('lets a re-claimed instance go to setup', () => {
		expect(leaveDecision({ ...base, to: at('/setup'), signIn: 'setup' })).toBe('setup');
		expect(leaveDecision({ ...base, to: at('/login'), signIn: 'setup' })).toBe('prompt');
		expect(leaveDecision({ ...base, to: at('/setup') })).toBe('prompt');
	});
});
