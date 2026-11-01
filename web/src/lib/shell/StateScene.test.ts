import { afterEach, describe, expect, it, vi } from 'vitest';
import { render } from '@testing-library/svelte';
import { scenes } from '$lib/brand/scenes.generated';
import StateScene from './StateScene.svelte';

const id = scenes[0]!.id;
const KEY = 'lodestar.appearance';
const cache = (illustrations: boolean) =>
	localStorage.setItem(
		KEY,
		JSON.stringify({ density: 'comfortable', motion: 'system', illustrations })
	);

afterEach(() => {
	localStorage.clear();
	vi.unstubAllGlobals();
});

describe('StateScene', () => {
	it('renders the picture when illustrations are on', () => {
		cache(true);
		expect(render(StateScene, { id }).container.querySelector('img')).not.toBeNull();
	});

	it('renders the picture before any preference is cached', () => {
		expect(render(StateScene, { id }).container.querySelector('img')).not.toBeNull();
	});

	it('renders nothing when illustrations are off', () => {
		cache(false);
		expect(render(StateScene, { id }).container.querySelector('img, picture')).toBeNull();
	});

	it('reads the preference each time a scene is built', () => {
		cache(false);
		expect(render(StateScene, { id }).container.querySelector('img')).toBeNull();
		cache(true);
		expect(render(StateScene, { id }).container.querySelector('img')).not.toBeNull();
	});

	it('renders nothing under Save-Data, even with no cache, and leaves the preference alone', () => {
		vi.stubGlobal('navigator', { connection: { saveData: true } });
		const none = render(StateScene, { id });
		expect(none.container.querySelector('img, picture')).toBeNull();
		cache(true);
		const stored = localStorage.getItem(KEY);
		expect(render(StateScene, { id }).container.querySelector('img, picture')).toBeNull();
		expect(localStorage.getItem(KEY)).toBe(stored);
	});
});
