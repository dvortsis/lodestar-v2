// Diagnostics: what is copied is exactly what is shown, and a failed copy says so.
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { beforeEach, expect, it, vi } from 'vitest';

const copyText = vi.fn();
vi.mock('$lib/util/clipboard', () => ({ copyText: (t: string) => copyText(t) }));

import Diagnostics from './Diagnostics.svelte';
import type { System } from './logic';

const info = {
	build: {
		commit: null,
		date: null,
		rustc: null,
		target: 'x86_64-unknown-linux-musl',
		ui_build_id: null
	},
	channel: 'stable',
	data: { dir: '/home/someone/lodestar' },
	data_formats: { index: 'v3-t2', rebuild: null, store_schema: 12 },
	deployment: { image_digest: null, image_tag: null, kind: 'docker' },
	diagnostics: {
		changed: [{ name: 'network.port', source: 'env' }],
		logging: 'normal',
		recent_errors: 0
	},
	host: { arch: 'x86_64', cpus: 2, memory_bytes: null, os: 'linux', time_zone: 'UTC' },
	repository: 'https://github.com/example/lodestar',
	uptime_s: 5,
	version: '0.5.2'
} as unknown as System;

beforeEach(() => copyText.mockReset());

it('copies exactly the text it shows', async () => {
	copyText.mockResolvedValue('copied');
	render(Diagnostics, { info, uiBuild: 'ui-1' });
	const shown = screen.getByTestId('about-diagnostics').textContent;
	expect(shown).toContain('network.port (env)');
	expect(shown).not.toContain('/home/someone');
	await fireEvent.click(screen.getByRole('button', { name: 'Copy diagnostics' }));
	await waitFor(() => expect(copyText).toHaveBeenCalledWith(shown));
	expect(await screen.findByText('Copied to the clipboard')).toBeTruthy();
});

it('says so when neither copy path works', async () => {
	copyText.mockResolvedValue('manual');
	render(Diagnostics, { info, uiBuild: 'ui-1' });
	await fireEvent.click(screen.getByRole('button', { name: 'Copy diagnostics' }));
	expect(await screen.findByText(/Select the text and press Ctrl\+C/)).toBeTruthy();
});
