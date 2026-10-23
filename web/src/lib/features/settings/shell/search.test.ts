import { beforeAll, describe, expect, it } from 'vitest';
import { loadSettingMessages } from '../form/messages';
import { screens as mine } from '../screens/screen';
import { buildRegistry, registered } from './registry';
import { buildItems, createIndex, normalize, wordsOf, type SearchItem } from './search';
import { goldenView, screen } from './testing';

const item = (id: string, label: string, over: Partial<SearchItem> = {}): SearchItem => ({
	id,
	kind: 'field',
	screenId: 's',
	label,
	crumb: 'System › S',
	names: [],
	...over
});
const found = (index: ReturnType<typeof createIndex>, q: string) =>
	index.search(q).map((h) => h.item.id);

describe('normalize and wordsOf', () => {
	it('lower-cases in the locale and drops accents', () => {
		expect(normalize('Éclair À LA Crème')).toBe('eclair a la creme');
		expect(normalize('ÅNGSTRÖM')).toBe('angstrom');
	});

	it('splits into words and keeps a whole name too', () => {
		expect(wordsOf('LODESTAR_DHT_PORT')).toEqual(
			expect.arrayContaining(['lodestar', 'dht', 'port'])
		);
		expect(wordsOf('dht.intensity')).toEqual(
			expect.arrayContaining(['dht', 'intensity', 'dht.intensity'])
		);
		expect(wordsOf('  ')).toEqual([]);
	});
});

describe('the matcher', () => {
	const index = createIndex([
		item('intensity', 'Intensity', {
			help: 'How hard the crawler works. Uses more CPU and bandwidth.',
			names: ['speed', 'rate', 'dht.intensity', 'LODESTAR_INTENSITY']
		}),
		item('port', 'DHT port', {
			help: 'The UDP port. Intensity of traffic is unaffected.',
			names: ['firewall']
		}),
		item('timeout', 'Fetch timeout', { help: 'Seconds before giving up.', group: 'Fetching' }),
		item('cafe', 'Café menu'),
		item('zh', '爬取强度', { help: '爬虫的工作强度。' })
	]);

	it('ignores case and accents', () => {
		expect(found(index, 'INTENSITY')[0]).toBe('intensity');
		expect(found(index, 'cafe')).toEqual(['cafe']);
		expect(found(index, 'CAFÉ')).toEqual(['cafe']);
	});

	it('matches word prefixes, not the middle of a word', () => {
		expect(found(index, 'inten')).toContain('intensity');
		expect(found(index, 'ensity')).toEqual([]);
		expect(found(index, 'fetch time')).toEqual(['timeout']);
	});

	it('needs every word to match', () => {
		expect(found(index, 'dht port')).toEqual(['port']);
		expect(found(index, 'dht banana')).toEqual([]);
	});

	it('ranks a label match above a help match', () => {
		// "Intensity" is the label of one and in the help of another.
		expect(found(index, 'intensity')).toEqual(['intensity', 'port']);
	});

	it('finds by synonym, config key and variable name', () => {
		expect(found(index, 'speed')).toEqual(['intensity']);
		expect(found(index, 'dht.intens')[0]).toBe('intensity');
		expect(found(index, 'lodestar_intensity')).toEqual(['intensity']);
		expect(found(index, 'firewall')).toEqual(['port']);
	});

	it('finds by group', () => {
		expect(found(index, 'fetching')).toContain('timeout');
	});

	it('prefers an exact label, then a label that starts with the word', () => {
		const ix = createIndex([
			item('a', 'Port forwarding'),
			item('b', 'Port'),
			item('c', 'DHT port')
		]);
		expect(found(ix, 'port')).toEqual(['b', 'a', 'c']);
	});

	it('finds Chinese text by the characters, since there are no spaces to split on', () => {
		expect(found(index, '强度')).toEqual(['zh']);
		expect(found(index, '工作')).toEqual(['zh']);
	});

	it('returns nothing for an empty or symbol-only query', () => {
		expect(found(index, '')).toEqual([]);
		expect(found(index, ' ?! ')).toEqual([]);
	});
});

describe('the golden index', () => {
	const view = goldenView();
	const handBuilt = screen('handbuilt', {
		group: 'system',
		title: () => 'Hand built',
		search: () => [
			{
				id: 'one',
				label: () => 'Shortcut sheet',
				words: () => ['keys', 'cheat sheet'],
				anchor: 'sheet'
			}
		]
	});
	const registry = buildRegistry(
		view,
		[...mine, ...registered, handBuilt].filter((s, i, a) => a.indexOf(s) === i)
	);
	let index: ReturnType<typeof createIndex>;

	beforeAll(async () => {
		await loadSettingMessages(view.sections.map((s) => s.section));
		index = createIndex(buildItems(registry));
	});

	it('finds every field by its label', () => {
		const fields = buildItems(registry).filter((i) => i.kind === 'field');
		expect(fields.length).toBeGreaterThan(40);
		for (const f of fields) {
			expect(
				index.search(f.label).map((h) => h.item.id),
				f.id
			).toContain(f.id);
		}
	});

	it('finds every screen by its title and every hand-built entry by its label', () => {
		for (const i of buildItems(registry).filter((i) => i.kind !== 'field')) {
			expect(
				index.search(i.label).map((h) => h.item.id),
				i.id
			).toContain(i.id);
		}
		expect(index.search('cheat sheet')[0]!.item.anchor).toBe('sheet');
	});

	it('finds fields by the words people would use', () => {
		const ids = (q: string) => index.search(q).map((h) => h.item.id);
		expect(ids('port')).toEqual(expect.arrayContaining(['network.http_bind', 'network.dht_port']));
		expect(ids('nsfw')).toContain('content.show_adult');
		expect(ids('LODESTAR_DHT_PORT')).toContain('network.dht_port');
		expect(ids('dht.bind')).toContain('network.dht_port');
		expect(ids('fetch.timeout_secs')).toContain('crawler.timeout_secs');
	});

	it('gives a breadcrumb with the group, the screen and Advanced', () => {
		const byId = Object.fromEntries(index.items.map((i) => [i.id, i]));
		expect(byId['crawler.intensity']!.crumb).toBe('Crawling › Crawler');
		expect(byId['crawler.timeout_secs']!.crumb).toBe('Crawling › Crawler › Advanced');
		expect(byId['logging.verbosity']!.crumb).toBe('System › Logging');
	});
});
