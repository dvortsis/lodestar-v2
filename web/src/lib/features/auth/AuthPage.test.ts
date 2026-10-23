import { afterEach, describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import { tick } from 'svelte';
import { text } from '$lib/ui/testing';
import AuthPage from './AuthPage.svelte';

afterEach(() => (document.documentElement.style.colorScheme = ''));

describe('AuthPage', () => {
	it("follows the active theme's mode for the lockup", async () => {
		document.documentElement.style.colorScheme = 'light';
		render(AuthPage, { title: 'Sign in', lead: 'Lead', children: text('form') });
		const img = screen.getByRole('img', { name: 'Lodestar' }) as HTMLImageElement;
		expect(img.getAttribute('srcset')).toContain('horizontal_light');
		document.documentElement.style.colorScheme = 'dark';
		await new Promise((r) => setTimeout(r));
		await tick();
		expect(img.getAttribute('srcset')).toContain('horizontal_dark');
		expect(screen.getByRole('heading', { level: 1, name: 'Sign in' })).toBeTruthy();
	});
});
