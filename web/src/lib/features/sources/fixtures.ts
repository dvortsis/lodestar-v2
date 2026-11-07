// Tests only: a `GET /sources` row and its pieces.
// Only the API types: the e2e typecheck imports this before the messages are compiled.
import type { components } from '$lib/api/schema';

type Source = components['schemas']['Source'];

export const source = (over: Partial<Source> & { id: number; name: string }): Source => ({
	agents: { count: 1, enforced: false, max: 1 },
	config_schema: {
		$schema: 'https://json-schema.org/draft/2020-12/schema',
		type: 'object',
		properties: {},
		additionalProperties: false
	},
	consent: 'none',
	consent_required: false,
	consent_version: '',
	credential_spec: [],
	credentials: [],
	editable: ['agents', 'egress', 'enabled', 'priority', 'schedule'],
	egress: { live: null, setting: { route: 'direct' }, warnings: [] },
	enabled: false,
	endpoints: [],
	finds_private: false,
	health: { harvested_24h: 0, last_error: null, last_run: null, running: false, stored_24h: 0 },
	kinds: ['discovery'],
	needs: null,
	networks: ['bt'],
	not_yet_editable: ['purge'],
	password_required: false,
	plugin: `plugin-${over.id}`,
	priority: 'normal',
	purge_pending: false,
	restart_required: false,
	risk: 'low',
	schedule: { default: [], enforced: false, windows: [] },
	state: 'disabled',
	version: '0.0.0',
	...over
});

export const dht = source({
	id: 0,
	name: 'Mainline DHT',
	kinds: ['discovery', 'peer_finder'],
	enabled: true,
	state: 'enabled',
	health: {
		harvested_24h: 1840221,
		last_error: null,
		last_run: 1792677600,
		running: true,
		stored_24h: 30115
	}
});
export const nyaa = source({
	id: 1,
	name: 'Nyaa',
	kinds: ['discovery'],
	consent: 'external',
	consent_required: true,
	consent_version: 'v1',
	needs: 'endpoints_ack',
	endpoints: ['https://nyaa.example']
});
// Acknowledged earlier, then turned off: `needs` is null, `consent_required` stays true (a class fact).
export const nyaaAcked = source({
	...nyaa,
	needs: null,
	state: 'disabled'
});
export const prowlarr = source({
	id: 2,
	name: 'Prowlarr',
	kinds: ['importer'],
	consent: 'account',
	needs: 'credentials',
	credentials: ['api_key'],
	enabled: true,
	state: 'enabled',
	restart_required: true,
	health: { harvested_24h: 12, last_error: 'timeout', last_run: 1, running: false, stored_24h: 0 }
});
export const sourceList = (...sources: Source[]) => ({ sources });

// SRC-15b: one source of each consent class that asks for something.
export const lime = source({
	id: 3,
	name: 'LimeTorrents',
	kinds: ['discovery'],
	consent: 'restricted',
	consent_required: true,
	consent_version: 'v1-ab',
	needs: 'consent',
	password_required: true,
	endpoints: ['https://lime.example/search?q={q}']
});
export const darknet = source({
	id: 4,
	name: 'Darknet',
	kinds: ['network'],
	consent: 'overlay',
	consent_required: true,
	consent_version: 'v1-cd',
	needs: 'consent',
	password_required: true,
	networks: ['bt_i2p']
});
export const tracker = source({
	id: 5,
	name: 'Private Tracker',
	kinds: ['importer'],
	consent: 'account',
	needs: 'credentials',
	endpoints: ['https://tracker.example/api', 'https://tracker.example/rss']
});
