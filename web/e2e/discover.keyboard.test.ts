// S-discover, keyboard only: the tabs are links, j/k and the arrows move through Newest, Enter opens.
import {
	showAdultSettings,
	hash,
	hit,
	newest,
	response
} from '../src/lib/features/discover/testing';
import { expect, test } from './support/test';

test('Tab to the tabs, Enter on one, j and k through the rows, Enter opens the torrent', async ({
	page,
	go,
	kbd,
	mock
}) => {
	mock.on('getSettings', showAdultSettings(false));
	mock.on('getDiscover', (request) => {
		const movie = new URL(request.url()).searchParams.get('type') === 'movie';
		const first = movie ? 101 : 1;
		return response({
			newest: newest({
				hits: Array.from({ length: 12 }, (_, i) => hit(first + i)),
				total: 12,
				type: movie ? 'movie' : null
			})
		});
	});
	// The torrent page is not this slice's: whatever it asks for is refused, not left unmocked.
	await page.route('**/api/v1/torrents/**', (route) => route.abort());
	// The torrent page opens over Search, which loads its own (empty) results.
	mock.on('searchGet', {
		canonical: '',
		hits: [],
		limit: 50,
		next_cursor: null,
		notices: [],
		offset: 0,
		stemmed: false,
		took_ms: 1,
		total: 0,
		total_approx: false
	});

	await go('/discover');
	await expect(page.getByTestId(`discover-row-${hash(1)}`)).toBeVisible();

	await kbd.tabTo(page.getByTestId('discover-tab-movie'));
	await expect(page.getByTestId('discover-tab-movie')).toBeFocused();
	await kbd.press('Enter');
	await expect(page).toHaveURL(/type=movie/);
	await expect(page.getByTestId('discover-tab-movie')).toHaveAttribute('aria-current', 'page');
	await expect(page.getByTestId(`discover-row-${hash(101)}`)).toBeVisible();

	const list = page.getByRole('listbox', { name: 'Newest torrents' });
	await kbd.tabTo(list.getByRole('option').first());
	await expect(list.getByRole('option').first()).toBeFocused();
	await kbd.press('j');
	await expect(list.getByRole('option').nth(1)).toBeFocused();
	await kbd.press('j');
	await expect(list.getByRole('option').nth(2)).toBeFocused();
	await kbd.press('k');
	await expect(list.getByRole('option').nth(1)).toBeFocused();
	await kbd.press('ArrowDown');
	await expect(list.getByRole('option').nth(2)).toBeFocused();
	await kbd.press('Enter');
	await expect(page).toHaveURL(new RegExp(`/t/${hash(103)}`));
});

test('j and k do nothing while typing in a field', async ({ page, go, kbd, mock }) => {
	mock.on('getSettings', showAdultSettings(false));
	mock.on('getDiscover', response());
	await go('/discover');
	await expect(page.getByTestId(`discover-row-${hash(1)}`)).toBeVisible();
	// The registry's rule is global; the palette's input is the nearest text field.
	await kbd.press('Control+k');
	const input = page.getByRole('combobox', { name: 'Type a command' });
	await expect(input).toBeFocused();
	await page.keyboard.type('jk');
	await expect(input).toHaveValue('jk');
});
