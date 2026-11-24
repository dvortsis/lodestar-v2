// ENR-G on the OpenAPI mock: the adult-enrichment section (ENR-8) shows only when the API sends
// it, on the page and in the drawer. Without it nothing shows and its chunk never loads; with it,
// every value is text and only a safe https link is offered. Named *.routes.test.ts so the core
// project runs it (a *.spec.ts matches no project; scripts/e2e-spec-guard.ts).
import { readFileSync } from 'node:fs';
import type { Page } from '@playwright/test';
import type { components } from '../src/lib/api/schema';
import { expectNoA11yViolations } from './a11y';
import { HASH, detail, hit, mockTorrent, searchBody, siblings } from './detail.fixtures';
import type { OpenApiMock } from './mock/openapi-mock';
import { expect, test } from './support/test';

type Enrichment = components['schemas']['AdultEnrichment'];

const enrichment = (over: Partial<Enrichment> = {}): Enrichment => ({
	link: 'https://example.org/scenes/1',
	performers: ['Ada Example', 'Bo Sample'],
	performers_truncated: true,
	released: '2021-03-04',
	scene_title: 'A Scene',
	source: { id: 7, name: 'ThePornDB', plugin: 'tpdb' },
	studio: 'Example Studio',
	...over
});
const adultHit = hit({ content_type: 'adult', adult_kind: 'scene', title: 'Example scene' });

// The section's chunk, by its hashed name in the build the server serves. Read in the test, so
// listing the suite on a tree that isn't built still works.
const chunk = () =>
	(
		JSON.parse(readFileSync('.svelte-kit/output/client/.vite/manifest.json', 'utf8')) as Record<
			string,
			{ file: string }
		>
	)['src/lib/features/detail/adult/AdultEnrichment.svelte'].file;

const scripts = (page: Page) => {
	const seen: string[] = [];
	page.on('request', (r) => r.resourceType() === 'script' && seen.push(r.url()));
	return seen;
};
const loaded = (seen: string[]) => seen.filter((u) => u.endsWith(chunk()));

test('shows every field when the API sends the section, and is axe-clean', async ({
	page,
	go,
	mock
}) => {
	const seen = scripts(page);
	mockTorrent(mock, detail({ adult_enrichment: enrichment() }), adultHit);
	await go(`/t/${HASH}`);
	const section = page.getByTestId('detail-adult-enrichment');
	await expect(section).toBeVisible();
	await expect(page.getByTestId('detail-adult-studio')).toHaveText('Example Studio');
	await expect(page.getByTestId('detail-adult-released')).toHaveText('2021-03-04');
	await expect(page.getByTestId('detail-adult-scene')).toHaveText('A Scene');
	await expect(page.getByTestId('detail-adult-performers').locator('li')).toHaveText([
		'Ada Example',
		'Bo Sample'
	]);
	await expect(page.getByTestId('detail-adult-performers-more')).toBeVisible();
	await expect(page.getByTestId('detail-adult-source')).toHaveText('Matched by ThePornDB');
	const link = page.getByTestId('detail-adult-link');
	await expect(link).toHaveAttribute('href', 'https://example.org/scenes/1');
	await expect(link).toHaveAttribute('target', '_blank');
	await expect(link).toHaveAttribute('rel', 'noopener noreferrer');
	expect(loaded(seen)).toHaveLength(1);
	await expectNoA11yViolations(page);
});

test('without the section nothing shows and its code never loads', async ({ page, go, mock }) => {
	const seen = scripts(page);
	// What the API sends with Show adult content off, to a public caller, or for a hidden item.
	mockTorrent(mock, detail(), adultHit);
	await go(`/t/${HASH}`);
	await expect(page.getByTestId('detail-size')).toBeVisible();
	await page.locator('[data-section="parsed"]').scrollIntoViewIfNeeded();
	await expect(page.getByTestId('detail-parsed-years')).toBeVisible();
	await expect(page.getByTestId('detail-adult-enrichment')).toHaveCount(0);
	expect(seen.length).toBeGreaterThan(0);
	expect(loaded(seen)).toEqual([]);
});

test('every value is text, and an unsafe link is not offered', async ({ page, go, mock }) => {
	const markup = '<img src=x onerror="document.title=1">';
	mockTorrent(
		mock,
		detail({
			adult_enrichment: enrichment({
				studio: markup,
				scene_title: '<b>bold</b>',
				performers: [markup, '   '],
				link: 'javascript:alert(1)'
			})
		}),
		adultHit
	);
	await go(`/t/${HASH}`);
	const section = page.getByTestId('detail-adult-enrichment');
	await expect(page.getByTestId('detail-adult-studio')).toHaveText(markup);
	await expect(page.getByTestId('detail-adult-scene')).toHaveText('<b>bold</b>');
	await expect(page.getByTestId('detail-adult-performers').locator('li')).toHaveText([markup]);
	await expect(section.locator('img, b')).toHaveCount(0);
	await expect(page.getByTestId('detail-adult-link')).toHaveCount(0);
	expect(await page.title()).not.toBe('1');
});

test.describe('the drawer on /dev/detail', () => {
	const A = 'a'.repeat(40);
	const serve = (mock: OpenApiMock, adult: Enrichment | undefined) => {
		mock.on('getTorrent', detail({ hash: A, adult_enrichment: adult }));
		mock.on('searchGet', searchBody([{ ...adultHit, hash: A }]));
		mock.on('getTorrentSiblings', siblings(A));
		mock.on('listTorrentFiles', { files: [], offset: 3, total: 3 });
	};
	const open = async (page: Page) => {
		await page.setViewportSize({ width: 1440, height: 900 });
		await page.getByTestId('open-a').focus();
		await page.keyboard.press('Enter');
		await expect(page.getByTestId('drawer')).toBeVisible();
		await expect(page.getByTestId('detail-size')).toBeVisible();
	};

	test('shows the section when it is sent', async ({ page, go, mock }) => {
		serve(mock, enrichment());
		await go('/dev/detail');
		await open(page);
		const drawer = page.getByTestId('drawer');
		await expect(drawer.getByTestId('detail-adult-enrichment')).toBeVisible();
		await expect(drawer.getByTestId('detail-adult-studio')).toHaveText('Example Studio');
	});

	test('shows nothing and loads nothing when it is not', async ({ page, go, mock }) => {
		const seen = scripts(page);
		serve(mock, undefined);
		await go('/dev/detail');
		await open(page);
		await expect(page.getByTestId('detail-adult-enrichment')).toHaveCount(0);
		expect(loaded(seen)).toEqual([]);
	});
});
