// A map that fails to load leaves its keys missing (the field's name shows), and concurrent
// loads of one map make one import.
import { describe, expect, it, vi } from 'vitest';

const imports = vi.hoisted(() => ({ count: 0 }));
vi.mock('./message-map', () => ({
	LOADERS: {
		broken: () => {
			imports.count++;
			return Promise.reject(new Error('chunk'));
		},
		fine: () => {
			imports.count++;
			return Promise.resolve({ MESSAGES: { settings_fine_a_label: () => 'A' } });
		}
	}
}));

import { fieldLabel, loadMessages, messagesLoaded, settingText } from './messages';
import { spec } from './testing/values';

describe('loadMessages', () => {
	it('loads a map once for concurrent callers', async () => {
		await Promise.all([loadMessages(['fine']), loadMessages(['fine'])]);
		await loadMessages(['fine']);
		expect(imports.count).toBe(1);
		expect(settingText('settings.fine.a.label')).toBe('A');
	});

	it('settles when a map fails, and the field shows its name', async () => {
		vi.spyOn(console, 'warn').mockImplementation(() => {});
		await expect(loadMessages(['broken'])).resolves.toBeUndefined();
		expect(messagesLoaded(['broken'])).toBe(true);
		const s = spec('broken.a', { labelKey: 'settings.broken.a.label' });
		expect(fieldLabel(s)).toBe('a');
	});
});
