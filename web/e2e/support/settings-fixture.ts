// The settings index (the backend's golden file) and a value set for the General section, so the
// smoke snapshots of /settings/general render real fields instead of an empty form.
import { readFileSync } from 'node:fs';
import type { components } from '../../src/lib/api/schema';
import type { OpenApiMock } from '../mock/openapi-mock';

type Index = components['schemas']['SettingsIndex'];

const raw = readFileSync(
	new URL('../../../crates/lodestar/src/settings/golden/settings_index.txt', import.meta.url),
	'utf8'
);
export const SETTINGS_INDEX: Index = JSON.parse(raw.slice(raw.indexOf('\n') + 1));

// data_dir is set by LODESTAR_DATA_DIR and read-only, as in the golden index.
const value = (v: unknown): components['schemas']['SettingValue'] => ({
	value: v,
	source: 'env',
	read_only: true,
	restart_required: true,
	env: 'LODESTAR_DATA_DIR'
});

/** Answers the index plus every section: General has its one field, others are empty. */
export function mockSettings(mock: OpenApiMock): void {
	mock.on('getAuthStatus', { logged_in: true, setup_required: false });
	mock.on('listSettings', SETTINGS_INDEX);
	mock.on('getSettings', (request) => {
		const section = new URL(request.url()).pathname.split('/').at(-1)!;
		return {
			status: 200,
			body: {
				section,
				revision: 3,
				restart_required: [],
				fields: section === 'general' ? { data_dir: value('/data') } : {}
			}
		};
	});
	mock.on('previewSettings', { sections: {} });
}
