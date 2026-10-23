<script lang="ts">
	// The grouped list of screens (§10.10.2): the left pane at x-large, the whole `/settings` page
	// below it. Items are links with an icon and a label, the current one `aria-current="page"`;
	// a badge is icon + text, and its text is part of the link's name ("Network & VPN, 1 problem").
	import { page } from '$app/state';
	import { m } from '$lib/paraglide/messages.js';
	import IconGear from '~icons/ph/gear';
	import { GROUPS, type ResolvedScreen, type ScreenGroup } from './registry';

	export interface Badge {
		/** The visible words ("1 problem", "Restart needed"). */
		text: string;
		/** Appended to the link's accessible name. */
		label: string;
		kind: 'problem' | 'restart' | 'notice';
	}

	interface Props {
		screens: readonly ResolvedScreen[];
		badges?: Record<string, Badge>;
		/** Called after a link is followed (a narrow layout closes nothing, tests watch it). */
		onnavigate?: () => void;
	}

	let { screens, badges = {}, onnavigate }: Props = $props();

	const heading: Record<ScreenGroup, () => string> = {
		preferences: m.settings_ui_group_preferences,
		crawling: m.settings_ui_group_crawling,
		library: m.settings_ui_group_library,
		connections: m.settings_ui_group_connections,
		system: m.settings_ui_group_system
	};

	const groups = $derived(
		GROUPS.map((g) => ({ group: g, items: screens.filter((s) => s.screen.group === g) })).filter(
			(g) => g.items.length > 0
		)
	);
	const current = $derived(page.params.section);
</script>

<nav aria-label={m.settings_ui_nav_label()} class="settings-nav" data-testid="settings-nav">
	{#each groups as g (g.group)}
		<h2 class="settings-nav-heading" id="settings-nav-{g.group}">{heading[g.group]()}</h2>
		<ul aria-labelledby="settings-nav-{g.group}">
			{#each g.items as r (r.screen.id)}
				{@const badge = badges[r.screen.id]}
				{@const Icon = r.screen.icon ?? IconGear}
				<li>
					<!-- eslint-disable svelte/no-navigation-without-resolve -- no base path (svelte.config.js) -->
					<a
						class="settings-nav-link"
						href="/settings/{r.screen.id}"
						aria-current={current === r.screen.id ? 'page' : undefined}
						data-testid="settings-nav-{r.screen.id}"
						onclick={onnavigate}
					>
						<Icon class="settings-nav-icon" aria-hidden="true" />
						<span class="settings-nav-label">
							{r.screen.title()}{#if badge}<span class="sr-only">, {badge.label}</span>{/if}
						</span>
						{#if badge}
							<span
								class="settings-nav-badge"
								data-kind={badge.kind}
								data-testid="settings-badge-{r.screen.id}"
								aria-hidden="true">{badge.text}</span
							>
						{/if}
					</a>
					<!-- eslint-enable svelte/no-navigation-without-resolve -->
				</li>
			{/each}
		</ul>
	{/each}
</nav>

<style>
	.settings-nav {
		display: flex;
		flex-direction: column;
		gap: calc(var(--spacing) * 1);
	}
	.settings-nav-heading {
		margin-top: calc(var(--spacing) * 4);
		padding-inline: calc(var(--spacing) * 3);
		font-size: var(--text-caption);
		font-weight: 600;
		color: var(--color-muted-text);
	}
	.settings-nav-heading:first-child {
		margin-top: 0;
	}
	ul {
		display: flex;
		flex-direction: column;
	}
	.settings-nav-link {
		display: flex;
		align-items: center;
		gap: calc(var(--spacing) * 3);
		min-height: var(--control-height);
		padding-inline: calc(var(--spacing) * 3);
		border-radius: var(--radius-field);
		color: var(--color-base-content);
		text-decoration: none;
	}
	@media (pointer: coarse) {
		.settings-nav-link {
			min-height: var(--target-min);
		}
	}
	.settings-nav-link:hover {
		background: var(--color-base-200);
	}
	.settings-nav-link[aria-current='page'] {
		background: var(--color-base-300);
		font-weight: 600;
	}
	.settings-nav-link:focus-visible {
		outline: var(--focus-ring-width) solid var(--color-primary);
		outline-offset: calc(var(--focus-ring-offset) * -1);
	}
	:global(.settings-nav-icon) {
		flex: none;
		width: var(--icon-size-nav);
		height: var(--icon-size-nav);
	}
	.settings-nav-label {
		flex: 1;
		min-width: 0;
	}
	.settings-nav-badge {
		flex: none;
		padding-inline: calc(var(--spacing) * 2);
		border-radius: var(--radius-selector);
		border: var(--border) solid currentColor;
		font-size: var(--text-caption);
		color: var(--color-base-content);
	}
	.settings-nav-badge[data-kind='problem'] {
		color: var(--color-error);
	}
	.settings-nav-badge[data-kind='restart'] {
		color: var(--color-warning);
	}
</style>
