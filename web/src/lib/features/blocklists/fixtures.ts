// Tests only: a `GET /filters/subscriptions` answer and its pieces.
import type {
	CatalogList,
	ConnectedList,
	Strictness,
	StrictnessList,
	Subscriptions
} from './lists';

export const figures = (over: Partial<StrictnessList> = {}): StrictnessList => ({
	categories: ['anti_p2p'],
	floor: false,
	hashes: null,
	ipv4_public_share: 0.31,
	ipv4_share: 0.2,
	ipv6_ranges: 0,
	kind: 'ip',
	measured: true,
	name: 'BT_BlockLists',
	...over
});

export const strictness = (over: Partial<Strictness> = {}): Strictness => ({
	categories: ['anti_p2p'],
	estimate: false,
	floor_hashes: 0,
	hashes: 0,
	ipv4_public_share: 0,
	ipv4_share: 0,
	ipv6_ranges: 0,
	ipv6_share: 0,
	level: 'none',
	lists: [],
	text: '',
	unknown: [],
	...over
});

export const catalog = (over: Partial<CatalogList> = {}): CatalogList => ({
	about: 'Blocks addresses seen sharing for anti-P2P outfits.',
	figures: figures(),
	format: 'p2p',
	key: 'bt_blocklists',
	kind: 'ip',
	name: 'BT_BlockLists',
	subscription: null,
	url: 'https://lists.example.org/bt.p2p.gz',
	...over
});

export const connected = (
	id = 4,
	over: Partial<ConnectedList['subscription']> = {},
	held = false
): ConnectedList => ({
	figures: figures({ measured: false, name: 'Mine' }),
	held,
	subscription: {
		action: 'drop',
		checked_at: 1,
		enabled: true,
		entries: 100,
		error: null,
		floor: false,
		format: 'p2p',
		id,
		kind: 'ip',
		name: 'Mine',
		status: held ? 'held' : 'ok',
		updated_at: 1_700_000_000,
		url: 'https://lists.example.org/mine.p2p',
		...over
	}
});

export const subscriptions = (over: Partial<Subscriptions> = {}): Subscriptions => ({
	catalog: [catalog()],
	connected: [],
	strictness: strictness(),
	...over
});
