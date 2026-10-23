// Tests only (vitest and Playwright): B2's golden `GET /settings` as an index, a view and values.
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import type { components } from '$lib/api/schema';
import { sv, values } from '../form/testing/values';
import type { SectionValues } from '../form/types';
import type { SystemInfo } from './system';

// Vitest and Playwright both run from web/ (import.meta.url isn't a file URL under Vite).
export const GOLDEN = resolve(
	process.cwd(),
	'../crates/lodestar/src/settings/golden/settings_index.txt'
);

export function goldenIndex(): components['schemas']['SettingsIndex'] {
	const text = readFileSync(GOLDEN, 'utf8');
	return JSON.parse(text.slice(text.indexOf('\n') + 1));
}

/** A saved-looking value for a field of the golden schema. */
function sample(p: Record<string, unknown>): unknown {
	// The second option when there is one: the first is often what an edit would pick.
	if (Array.isArray(p.enum)) return p.enum[Math.min(1, p.enum.length - 1)];
	switch (p.type) {
		case 'boolean':
			return false;
		case 'integer':
			return typeof p.minimum === 'number' ? p.minimum : 1;
		case 'object':
			return {};
		default:
			return typeof p.example === 'string' ? p.example : '';
	}
}

export function goldenValues(): SectionValues[] {
	return goldenIndex().sections.map((s) => {
		const props = (s.schema as unknown as { properties: Record<string, Record<string, unknown>> })
			.properties;
		return values(
			s.section,
			1,
			Object.fromEntries(
				s.fields.map((f) => [
					f.name,
					sv(sample(props[f.name]!), 'default', { read_only: f.read_only, env: f.env })
				])
			)
		);
	});
}

// --- `/system` and the network check ---------------------------------------------------------
/** A `/system` answer; only what the Settings shell reads is meaningful. */
export function systemInfo(over: Partial<SystemInfo> = {}): SystemInfo {
	return {
		address_change: null,
		channel: 'stable',
		connections: [],
		deployment: { image_digest: null, image_tag: null, kind: 'docker' },
		diagnostics: { changed: [], logging: null, recent_errors: null },
		fixture: false,
		repository: 'https://github.com/dvortsis/lodestar-v2',
		restart: { available: true, estimated_seconds: 20, hint: null, method: 'reexec' },
		restart_required: [],
		started_at: 1_000,
		uptime_s: 60,
		version: '0.5.0',
		...over
	} as unknown as SystemInfo;
}

export const checkBody = (over: Record<string, unknown> = {}) => ({
	checked_at: 1,
	external: { agreeing: 14, ip: '203.0.113.9', port: 6881, voters: 16 },
	inbound_queries: 412,
	mapping: 'unavailable',
	measured_s: 590,
	min_window_s: 120,
	reachable: true,
	reason: 'unsolicited_queries',
	unsolicited_queries: 37,
	unsolicited_sources: 21,
	window_s: 600,
	window_started_at: 1,
	...over
});
