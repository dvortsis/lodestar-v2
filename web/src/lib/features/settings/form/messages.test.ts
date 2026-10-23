import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { EN_FILE, MAP_FILE, messageKeys, renderMap } from './message-map.gen';
import { fieldHelp, fieldLabel, messageId, settingText, unitText } from './messages';
import { spec } from './testing/values';

afterEach(() => vi.restoreAllMocks());

describe('settingText', () => {
	it('maps a server key to its message id', () => {
		expect(messageId('settings.crawler.intensity.label')).toBe('settings_crawler_intensity_label');
		expect(messageId('settings.preferences.time-zone.help')).toBe(
			'settings_preferences_time_zone_help'
		);
	});

	it('resolves a settings message, and returns undefined for a missing one', () => {
		expect(settingText('settings.form.unit.seconds')).toBe('seconds');
		expect(settingText('settings.crawler.no_such_field.label')).toBeUndefined();
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
		const want = renderMap(messageKeys(readFileSync(EN_FILE, 'utf8')));
		expect(
			readFileSync(MAP_FILE, 'utf8'),
			'run: node --experimental-strip-types src/lib/features/settings/form/message-map.gen.ts'
		).toBe(want);
	});

	it('leaves out messages with placeholders', () => {
		expect(messageKeys('{"settings_a": "A", "settings_b": "{n} b", "other": "x"}')).toEqual([
			'settings_a'
		]);
		expect(renderMap([])).toContain('= {};');
	});
});
