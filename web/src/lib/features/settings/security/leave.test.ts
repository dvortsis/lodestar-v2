// The Security screen's leave rule against the engine's, over every case: they must agree.
import { describe, expect, it } from 'vitest';
import { leaveDecision } from '../form/leave';
import { securityLeave } from './leave';

const here = new URL('http://lodestar.test/settings/security');
const places: (URL | null)[] = [
	null,
	here,
	new URL('http://lodestar.test/settings/security#keys'),
	new URL('http://lodestar.test/settings/security?x=1'),
	new URL('http://lodestar.test/settings/general'),
	new URL('http://elsewhere.test/settings/security')
];

describe('securityLeave', () => {
	it('agrees with the engine over dirty × bypass × type × from × to', () => {
		let cases = 0;
		for (const dirty of [false, true])
			for (const bypass of [false, true])
				for (const type of ['link', 'goto', 'popstate', 'form', 'leave', 'enter'])
					for (const from of places)
						for (const to of places) {
							const input = { dirty, bypass, type, from, to };
							expect(securityLeave(input), JSON.stringify(input)).toBe(leaveDecision(input));
							cases++;
						}
		expect(cases).toBe(2 * 2 * 6 * places.length ** 2);
	});

	it('asks for a dirty move to another screen, lets the browser ask on close', () => {
		const base = { dirty: true, bypass: false, from: here };
		expect(securityLeave({ ...base, type: 'link', to: places[4]! })).toBe('prompt');
		expect(securityLeave({ ...base, type: 'leave', to: null })).toBe('browser');
		expect(securityLeave({ ...base, type: 'link', to: places[2]! })).toBe('allow');
	});
});
