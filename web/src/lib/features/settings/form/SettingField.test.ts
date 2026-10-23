import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import SettingField from './SettingField.svelte';
import { fakeServer } from './testing/fake-server';
import { createSettingsForm, type SettingsForm } from './form.svelte';
import { spec, sv, values } from './testing/values';
import type { FieldKey, FieldSpec } from './types';
import { registerWidget } from './widgets';
import WidgetStub from './testing/WidgetStub.svelte';

const SPECS: FieldSpec[] = [
	spec('crawler.timeout_secs', {
		kind: 'integer',
		minimum: 1,
		maximum: 120,
		unit: 'seconds',
		restartRequired: true,
		config: 'fetch.timeout_secs'
	}),
	spec('crawler.intensity', { kind: 'enum', enum: ['low', 'balanced', 'aggressive'] }),
	spec('crawler.encryption', { kind: 'enum', enum: ['prefer', 'require'], widget: 'radio' }),
	spec('crawler.max_sockets', {
		kind: 'integer',
		minimum: 0,
		maximum: 4096,
		auto: 0,
		preview: true,
		unit: 'connections'
	}),
	spec('crawler.paused', { kind: 'boolean' }),
	spec('network.dht_port', {
		kind: 'integer',
		minimum: 1,
		maximum: 65535,
		env: 'LODESTAR_DHT_PORT'
	}),
	spec('network.http_bind', { format: 'socket-addr' }),
	spec('general.data_dir', { readOnly: true }),
	spec('secrets.token', { writeOnly: true }),
	spec('logging.events', { kind: 'map', widget: 'log-events' }),
	spec('logging.mystery', { kind: 'unknown' }),
	spec('logging.tags', { kind: 'string-list' })
];

const initial = () => [
	values('crawler', 3, {
		timeout_secs: sv(12, 'db'),
		intensity: sv('balanced'),
		encryption: sv('prefer'),
		max_sockets: sv(0),
		paused: sv(false)
	}),
	values('network', 2, {
		dht_port: sv(6881, 'env', { env: 'LODESTAR_DHT_PORT', read_only: true }),
		http_bind: sv('0.0.0.0:8090', 'file')
	}),
	values('general', 1, { data_dir: sv('/data', 'default', { read_only: true }) }),
	values('secrets', 1, { token: sv(null, 'db') }),
	values('logging', 1, { events: sv({ a: true }), mystery: sv([1, 2]), tags: sv(['x']) })
];

let server: ReturnType<typeof fakeServer>;
let form: SettingsForm;

beforeEach(async () => {
	vi.spyOn(console, 'warn').mockImplementation(() => {});
	sessionStorage.clear();
	server = fakeServer(initial());
	vi.stubGlobal('fetch', server.fetch);
	form = createSettingsForm({ id: 'field-test', fields: SPECS, href: (k) => `/settings/x#${k}` });
	await waitFor(() => expect(form.status).toBe('ready'));
});

afterEach(() => {
	form.destroy();
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
});

const show = (key: FieldKey) => render(SettingField, { form, key });
const root = (key: FieldKey) => document.getElementById(key)!;

describe('SettingField', () => {
	it('labels the control, puts the unit inside it and links help', () => {
		show('crawler.timeout_secs');
		const input = screen.getByLabelText('timeout_secs') as HTMLInputElement;
		expect(input.value).toBe('12');
		expect(within(root('crawler.timeout_secs')).getByText('seconds')).toBeTruthy();
		const described = input.getAttribute('aria-describedby')!.split(' ');
		expect(described.map((id) => document.getElementById(id)?.textContent)).toContain('seconds');
	});

	it('anchors the field for #section.field deep links', () => {
		show('crawler.timeout_secs');
		expect(root('crawler.timeout_secs').dataset.field).toBe('crawler.timeout_secs');
	});

	it('shows an error inline with aria-invalid, after blur', async () => {
		show('crawler.timeout_secs');
		const input = screen.getByLabelText('timeout_secs');
		await fireEvent.input(input, { target: { value: '500' } });
		await fireEvent.blur(input);
		expect(input.getAttribute('aria-invalid')).toBe('true');
		const ids = input.getAttribute('aria-describedby')!.split(' ');
		const texts = ids.map((id) => document.getElementById(id)?.textContent ?? '').join(' ');
		expect(texts).toContain('Must be between 1 and 120 seconds.');
	});

	it('marks a changed value with an accessible dot', () => {
		show('crawler.timeout_secs');
		expect(
			within(root('crawler.timeout_secs')).getByRole('img', { name: 'Changed from default' })
		).toBeTruthy();
		show('crawler.intensity');
		expect(within(root('crawler.intensity')).queryByRole('img')).toBeNull();
	});

	it('shows each status line where it applies', () => {
		show('crawler.timeout_secs');
		expect(within(root('crawler.timeout_secs')).getByText('Applies after restart')).toBeTruthy();
		show('network.dht_port');
		const env = within(root('network.dht_port')).getByTestId('status-env');
		expect(env.textContent).toContain('Set by LODESTAR_DHT_PORT');
		expect(env.textContent).toContain('container template');
		expect(within(env).getByRole('button', { name: 'Copy LODESTAR_DHT_PORT' })).toBeTruthy();
		expect((screen.getByLabelText('dht_port') as HTMLInputElement).disabled).toBe(true);
		show('general.data_dir');
		const ro = within(root('general.data_dir')).getByTestId('status-read-only');
		expect(within(ro).getByRole('button', { name: 'Copy the value of data_dir' })).toBeTruthy();
		show('network.http_bind');
		expect(within(root('network.http_bind')).getByTestId('status-file').textContent).toContain(
			'From config.toml'
		);
	});

	it('copies the variable name', async () => {
		const writeText = vi.fn(async () => {});
		vi.stubGlobal('navigator', { ...navigator, clipboard: { writeText } });
		show('network.dht_port');
		await fireEvent.click(screen.getByRole('button', { name: 'Copy LODESTAR_DHT_PORT' }));
		expect(writeText).toHaveBeenCalledWith('LODESTAR_DHT_PORT');
	});

	it('offers Reset only for a value saved in the app, and shows the keys', async () => {
		show('crawler.timeout_secs');
		await fireEvent.pointerDown(screen.getByRole('button', { name: 'Options for timeout_secs' }), {
			button: 0,
			pointerType: 'mouse'
		});
		const items = await screen.findAllByRole('menuitem');
		expect(items.map((i) => i.textContent?.trim())).toEqual([
			'Reset to default',
			'Copy link to this setting',
			'Show config.toml key and variable'
		]);
		await fireEvent.click(items[0]!);
		await waitFor(() => expect(form.field('crawler.timeout_secs').pendingReset).toBe(true));
		expect(screen.getByText('Goes back to the default when you save.')).toBeTruthy();
	});

	it('hides Reset for a default, file or locked value', async () => {
		show('network.http_bind');
		await fireEvent.pointerDown(screen.getByRole('button', { name: 'Options for http_bind' }), {
			button: 0,
			pointerType: 'mouse'
		});
		const items = await screen.findAllByRole('menuitem');
		expect(items.map((i) => i.textContent?.trim())).toEqual(['Copy link to this setting']);
	});

	it('shows config.toml key and variable on request', async () => {
		show('crawler.timeout_secs');
		await fireEvent.pointerDown(screen.getByRole('button', { name: 'Options for timeout_secs' }), {
			button: 0,
			pointerType: 'mouse'
		});
		await fireEvent.click(await screen.findByRole('menuitem', { name: /Show config.toml/ }));
		expect((await screen.findByTestId('field-keys')).textContent).toContain('fetch.timeout_secs');
	});

	it('switches Automatic on and off, showing the effective value', async () => {
		server.onPreview(() => ({
			status: 200,
			body: {
				sections: {
					crawler: {
						revision: 3,
						fields: {
							max_sockets: {
								estimates: [{ name: 'sockets', value: 400 }],
								restart_required: true,
								value: 0,
								warnings: []
							}
						}
					}
				}
			}
		}));
		show('crawler.max_sockets');
		const auto = screen.getByRole('switch', { name: 'Automatic' }) as HTMLInputElement;
		expect(auto.checked).toBe(true);
		expect(screen.queryByLabelText('max_sockets')).toBeNull();
		await waitFor(() =>
			expect(screen.getByTestId('automatic-value').textContent).toContain('Automatic · 400')
		);
		await fireEvent.click(auto);
		const input = (await screen.findByLabelText('max_sockets')) as HTMLInputElement;
		expect(input.value).toBe('400');
		expect(form.field('crawler.max_sockets').value).toBe(400);
		await fireEvent.click(auto);
		expect(form.field('crawler.max_sockets').dirty).toBe(false);
	});

	it('turns Automatic off with an empty, invalid input when there’s no estimate', async () => {
		show('crawler.max_sockets');
		await fireEvent.click(screen.getByRole('switch', { name: 'Automatic' }));
		const input = (await screen.findByLabelText('max_sockets')) as HTMLInputElement;
		expect(input.value).toBe('');
		form.blur('crawler.max_sockets');
		await waitFor(() => expect(input.getAttribute('aria-invalid')).toBe('true'));
	});

	it('shows a secret as Set, with Replace and Remove', async () => {
		show('secrets.token');
		const r = root('secrets.token');
		expect(within(r).getByTestId('secret-status').textContent).toContain('Set');
		expect(r.querySelector('input')).toBeNull(); // never pre-filled, never shown
		await fireEvent.click(within(r).getByRole('button', { name: 'Replace' }));
		const input = r.querySelector('input')!;
		expect(input.type).toBe('password');
		expect(input.autocomplete).toBe('new-password');
		expect(input.value).toBe('');
		await fireEvent.input(input, { target: { value: 'hunter2' } });
		expect(form.dirtyCount).toBe(1);
		expect(within(r).getByRole('button', { name: 'Remove' })).toBeTruthy();
	});

	it('removes a secret only after its confirmation, focused on Cancel', async () => {
		show('secrets.token');
		await fireEvent.click(within(root('secrets.token')).getByRole('button', { name: 'Remove' }));
		const dialog = await screen.findByRole('dialog');
		expect(within(dialog).getByText(/can't be undone/)).toBeTruthy();
		await waitFor(() => expect(document.activeElement?.textContent?.trim()).toBe('Cancel'));
		await fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(server.requests('PATCH', '/settings')).toHaveLength(0);
		await fireEvent.click(within(root('secrets.token')).getByRole('button', { name: 'Remove' }));
		const again = await screen.findByRole('dialog');
		await fireEvent.click(within(again).getByRole('button', { name: 'Remove' }));
		await waitFor(() => expect(server.requests('PATCH', '/settings')).toHaveLength(1));
		expect(server.requests('PATCH', '/settings')[0]!.body).toEqual({
			secrets: { if_revision: 1, token: null }
		});
	});

	it('shows a conflict with Keep mine and Use theirs', async () => {
		show('crawler.intensity');
		form.set('crawler.intensity', 'low');
		server.change('crawler', { intensity: { value: 'aggressive', source: 'db' } });
		await form.save();
		const c = await screen.findByTestId('conflict');
		expect(c.textContent).toContain('Changed elsewhere to aggressive');
		await fireEvent.click(within(c).getByRole('button', { name: 'Use theirs' }));
		expect(form.conflicts).toEqual([]);
		expect(form.field('crawler.intensity').value).toBe('aggressive');
	});

	it('keeps mine on Keep mine', async () => {
		show('crawler.intensity');
		form.set('crawler.intensity', 'low');
		server.change('crawler', { intensity: { value: 'aggressive', source: 'db' } });
		await form.save();
		await fireEvent.click(await screen.findByRole('button', { name: 'Keep mine' }));
		expect(form.field('crawler.intensity')).toMatchObject({ value: 'low', dirty: true });
	});

	it('renders segmented, radio, toggle, chips and the JSON fallback', async () => {
		show('crawler.intensity');
		const seg = screen.getByRole('radiogroup', { name: 'intensity' });
		await fireEvent.click(within(seg).getByRole('radio', { name: 'low' }));
		expect(form.field('crawler.intensity').value).toBe('low');

		show('crawler.encryption');
		await fireEvent.click(screen.getByRole('radio', { name: 'require' }));
		expect(form.field('crawler.encryption').value).toBe('require');

		show('crawler.paused');
		await fireEvent.click(screen.getByRole('switch', { name: 'paused' }));
		expect(form.field('crawler.paused').value).toBe(true);

		show('logging.tags');
		const tags = root('logging.tags');
		const add = within(tags).getByRole('textbox');
		await fireEvent.input(add, { target: { value: 'y' } });
		await fireEvent.click(within(tags).getByRole('button', { name: 'Add' }));
		expect(form.field('logging.tags').value).toEqual(['x', 'y']);
		await fireEvent.click(within(tags).getByRole('button', { name: 'Remove x' }));
		expect(form.field('logging.tags').value).toEqual(['y']);

		show('logging.mystery');
		const json = within(root('logging.mystery')).getByRole('textbox') as HTMLTextAreaElement;
		expect(JSON.parse(json.value)).toEqual([1, 2]);
		await fireEvent.input(json, { target: { value: '[1, ' } });
		expect(within(root('logging.mystery')).getByText(/isn't valid JSON/)).toBeTruthy();
		await fireEvent.input(json, { target: { value: '[3]' } });
		expect(form.field('logging.mystery').value).toEqual([3]);
	});

	it('renders a registered widget with the form, key and control props', () => {
		const off = registerWidget('log-events', WidgetStub);
		show('logging.events');
		const stub = within(root('logging.events')).getByTestId('widget-stub');
		expect(stub.dataset.key).toBe('logging.events');
		expect(stub.id).toBeTruthy();
		off();
	});

	it('says Saved, then Saved · applies after restart', async () => {
		show('crawler.intensity');
		show('crawler.timeout_secs');
		server.next('PATCH /settings', () => ({
			status: 200,
			body: {
				sections: {
					crawler: values(
						'crawler',
						4,
						{
							timeout_secs: sv(30, 'db'),
							intensity: sv('low', 'db'),
							encryption: sv('prefer'),
							max_sockets: sv(0),
							paused: sv(false)
						},
						['timeout_secs']
					)
				}
			}
		}));
		form.set('crawler.intensity', 'low');
		form.set('crawler.timeout_secs', 30);
		await form.save();
		await waitFor(() =>
			expect(
				within(root('crawler.timeout_secs')).getByText('Saved · applies after restart')
			).toBeTruthy()
		);
		expect(within(root('crawler.intensity')).getByText('Saved')).toBeTruthy();
	});
});
