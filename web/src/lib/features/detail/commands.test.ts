import { beforeEach, describe, expect, it, vi } from 'vitest';

const copyMagnet = vi.fn();
const openMagnet = vi.fn();
vi.mock('./magnet', () => ({ copyMagnet, openMagnet }));
// One private registry for the file; the module under test registers into it once.
vi.mock('$lib/keys', async (original) => {
	const real = await original<typeof import('$lib/keys')>();
	return { ...real, keys: real.createKeys({ mac: false }) };
});

const { keys } = await import('$lib/keys');
const commands = await import('./commands');

const press = (key: string) =>
	keys.handle(new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true }));
const ROW = 'a'.repeat(40);
const DRAWER = 'b'.repeat(40);

beforeEach(() => {
	copyMagnet.mockReset();
	openMagnet.mockReset();
	commands.setCommandTarget(null);
	commands.setDrawerTarget(null);
});

describe('detail commands', () => {
	it('registers c and o once, however often it is called', async () => {
		const spy = vi.spyOn(keys, 'register');
		await Promise.all([commands.registerDetailCommands(), commands.registerDetailCommands()]);
		await commands.registerDetailCommands();
		// Registered by the first call only (the spy was installed before it).
		expect(spy).toHaveBeenCalledTimes(2);
		const ours = keys.list().filter((c) => c.id.startsWith('detail.'));
		expect(ours.map((c) => [c.id, c.keys, c.scope])).toEqual([
			['detail.copy_magnet', ['c'], 'global'],
			['detail.open_magnet', ['o'], 'global']
		]);
	});

	it('does nothing, and is inactive, without a target', () => {
		expect(keys.list().find((c) => c.id === 'detail.copy_magnet')!.active).toBe(false);
		expect(press('c')).toBe(false);
		expect(press('o')).toBe(false);
		expect(copyMagnet).not.toHaveBeenCalled();
	});

	it('acts on the focused row when no drawer is open', () => {
		commands.setCommandTarget(ROW);
		expect(keys.list().find((c) => c.id === 'detail.open_magnet')!.active).toBe(true);
		expect(press('c')).toBe(true);
		expect(copyMagnet).toHaveBeenCalledWith({ hash: ROW });
		expect(press('o')).toBe(true);
		expect(openMagnet).toHaveBeenCalledWith({ hash: ROW });
	});

	it('prefers the open drawer over the row', () => {
		commands.setCommandTarget(ROW);
		commands.setDrawerTarget(DRAWER);
		press('c');
		expect(copyMagnet).toHaveBeenCalledWith({ hash: DRAWER });
		expect(commands.commandTarget()).toBe(DRAWER);
		commands.setDrawerTarget(null);
		expect(commands.commandTarget()).toBe(ROW);
	});

	it('stays quiet while typing in a field', () => {
		commands.setCommandTarget(ROW);
		const input = document.createElement('input');
		document.body.append(input);
		const event = new KeyboardEvent('keydown', { key: 'c', bubbles: true, cancelable: true });
		Object.defineProperty(event, 'target', { value: input });
		expect(keys.handle(event)).toBe(false);
		expect(copyMagnet).not.toHaveBeenCalled();
	});
});
