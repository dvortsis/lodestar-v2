import { expect, it } from 'vitest';
import { NAV_ICONS } from './nav-icons';
import { screens } from './screens';

it('gives an icon only to registered screens that have none of their own', () => {
	for (const id of Object.keys(NAV_ICONS)) {
		const screen = screens.find((s) => s.id === id);
		expect(screen, id).toBeDefined();
		expect(screen!.icon ?? screen!.iconLoad, `${id} already has an icon`).toBeUndefined();
	}
});

it('leaves no registered screen on the gear except General', () => {
	const gear = screens.filter((s) => !s.icon && !s.iconLoad && !NAV_ICONS[s.id]).map((s) => s.id);
	expect(gear).toEqual([]);
});
