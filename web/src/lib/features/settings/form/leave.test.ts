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
});
