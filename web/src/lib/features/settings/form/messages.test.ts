import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { basename, dirname, join } from 'node:path';
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
	EN_FILE,
	MAP_DIR,
	messageKeys,
	renderAll,
	renderIndex,
	splitUnits,
	unitOf
} from './message-map.gen';
import {
	fieldHelp,
	fieldLabel,
	LOADED,
	loadSettingMessages,
	messageId,
	settingText,
	unitText
} from './messages';
import { spec } from './testing/values';

afterEach(() => vi.restoreAllMocks());

describe('settingText', () => {
	it('maps a server key to its message id', () => {
		expect(messageId('settings.crawler.intensity.label')).toBe('settings_crawler_intensity_label');
		expect(messageId('settings.preferences.time-zone.help')).toBe(
			'settings_preferences_time_zone_help'
		);
	});

	it('resolves a loaded settings message, and returns undefined for a missing one', () => {
		LOADED.set('settings_crawler_intensity_label', () => 'Intensity');
		try {
			expect(settingText('settings.crawler.intensity.label')).toBe('Intensity');
		} finally {
			LOADED.delete('settings_crawler_intensity_label');
		}
		expect(settingText('settings.crawler.no_such_field.label')).toBeUndefined();
		// The engine's own strings aren't server keys.
		expect(settingText('settings.form.unit.seconds')).toBeUndefined();
	});

	it('never resolves a message outside settings_*', () => {
		expect(settingText('common.dismiss')).toBeUndefined();
		expect(settingText('auth.back')).toBeUndefined();
	});

	it('falls back to the field name, and shows no help, warning in dev', () => {
		const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
		const s = spec('crawler.brand_new', { labelKey: 'settings.crawler.brand_new.label' });
		expect(fieldLabel(s)).toBe('brand_new');
		expect(fieldHelp(s)).toBeUndefined();
		expect(warn).toHaveBeenCalledWith(expect.stringContaining('settings.crawler.brand_new.label'));
	});
});

describe('loadSettingMessages', () => {
	afterEach(() => LOADED.clear());

	it('loads a section’s messages and the shared group headings, and no other section', async () => {
		expect(settingText('settings.crawler.intensity.label')).toBeUndefined();
		await loadSettingMessages(['crawler']);
		expect(settingText('settings.crawler.intensity.label')).toBe('Intensity');
		expect(settingText('settings.crawler.intensity.option.balanced')).toBe('Balanced');
		expect(settingText('settings.group.fetching')).toBe('Fetching torrent details');
		expect(settingText('settings.network.http_bind.label')).toBeUndefined();
	});

	it('reads as missing for a section the build does not know, without failing', async () => {
		await expect(loadSettingMessages(['no_such_section'])).resolves.toBeUndefined();
		expect(settingText('settings.no_such_section.label')).toBeUndefined();
	});

	it('loads each section once', async () => {
		await Promise.all([loadSettingMessages(['network']), loadSettingMessages(['network'])]);
		expect(settingText('settings.network.http_bind.label')).toBe('Web address');
	});
});

describe('unitText', () => {
	it('names each known unit, and an unknown one as sent', () => {
		const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
		expect(['seconds', 'mebibytes', 'connections', 'files', 'days'].map(unitText)).toEqual([
			'seconds',
			'MiB',
			'connections',
			'files',
			'days'
		]);
		expect(unitText('per hour')).toBe('per hour');
		expect(warn).toHaveBeenCalledWith(expect.stringContaining('per hour'));
		expect(unitText(undefined)).toBeUndefined();
	});
});

describe('the bundle', () => {
	it('never indexes the message namespace dynamically', () => {
		// `m[key]` would pull every message of the app into the Settings chunk.
		const dir = import.meta.dirname;
		const sources = readdirSync(dir).filter(
			(f) => /\.(ts|svelte)$/.test(f) && !f.endsWith('.test.ts') && f !== 'message-map.ts'
		);
		expect(sources.length).toBeGreaterThan(5);
		for (const f of sources) {
			expect(readFileSync(join(dir, f), 'utf8'), f).not.toMatch(/\bm\s*\[/);
		}
	});
});

describe('the message map', () => {
	it('is up to date with messages/en.json', () => {
		const want = renderAll(readFileSync(EN_FILE, 'utf8'));
		const hint =
			'run: node --experimental-strip-types src/lib/features/settings/form/message-map.gen.ts';
		for (const [file, text] of want) expect(readFileSync(file, 'utf8'), hint).toBe(text);
		// No unit left over from a section that no longer exists.
		const units = [...want.keys()].filter((f) => dirname(f) === MAP_DIR).map((f) => basename(f));
		expect(existsSync(MAP_DIR) ? readdirSync(MAP_DIR).sort() : [], hint).toEqual(units.sort());
	});

	it('leaves out messages with placeholders, the engine’s own and the screens’ strings', () => {
		expect(
			messageKeys(
				'{"settings_a": "A", "settings_b": "{n} b", "settings_form_c": "C", "settings_ui_d": "D", "other": "x"}'
			)
		).toEqual(['settings_a']);
		expect(renderIndex([])).toContain('= {};');
	});

	it('splits messages by section, with the group headings in common', () => {
		expect(unitOf('settings_group_dht')).toBe('common');
		expect(unitOf('settings_network_label')).toBe('common');
		expect(unitOf('settings_network_help')).toBe('common');
		expect(unitOf('settings_network_http_bind_label')).toBe('network');
		// A field named like a section's label is still a field.
		expect(unitOf('settings_network_label_help')).toBe('network');
		expect([
			...splitUnits(['settings_b_x', 'settings_a_x', 'settings_group_g', 'settings_a_y'])
		]).toEqual([
			['a', ['settings_a_x', 'settings_a_y']],
			['b', ['settings_b_x']],
			['common', ['settings_group_g']]
		]);
	});
});
