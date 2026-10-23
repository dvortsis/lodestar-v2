<script lang="ts">
	// The search box's results (lazy): each with its breadcrumb, the current value and where the
	// value comes from. Options, not links: the box keeps focus and the arrows move through them.
	import { m } from '$lib/paraglide/messages.js';
	import Mono from '$lib/ui/labels/Mono.svelte';
	import { displayValue } from '../form/display';
	import type { SectionValues } from '../form/types';
	import type { Hit } from './search';

	interface Props {
		hits: readonly Hit[];
		query: string;
		activeId?: string;
		/** Saved values by section, fetched for the sections the hits are in. */
		values: Record<string, SectionValues | undefined>;
		/** The Settings user guide, when the server names its repository. */
		guideUrl?: string;
		onpick: (hit: Hit) => void;
		onactive: (id: string) => void;
	}

	let { hits, query, activeId, values, guideUrl, onpick, onactive }: Props = $props();

	function source(s: string, env: string | null): string | undefined {
		switch (s) {
			case 'default':
				return m.setting_default();
			case 'file':
				return m.setting_file();
			case 'db':
				return m.setting_saved();
			case 'env':
				return m.setting_env({ variable: env ?? '' });
			default:
				return undefined;
		}
	}

	function valueOf(hit: Hit) {
		const spec = hit.item.spec;
		if (!spec) return undefined;
		const v = values[spec.section]?.fields[spec.name];
		if (!v) return undefined;
		return {
			text: spec.writeOnly ? '—' : displayValue(spec, v.value),
			mono: spec.kind === 'string' && typeof v.value === 'string' && v.value !== '',
			source: source(v.source, v.env)
		};
	}
</script>

{#if hits.length === 0}
	<div class="search-none" data-testid="settings-search-none">
		<p>{m.settings_ui_search_none({ query })}</p>
		{#if guideUrl}
			<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -- an external page, opened in a new tab -->
			<a href={guideUrl} target="_blank" rel="noopener noreferrer">{m.settings_ui_search_guide()}</a
			>
		{/if}
	</div>
{:else}
	<ul
		id="settings-search-list"
		role="listbox"
		aria-label={m.settings_ui_search_results()}
		class="search-list"
	>
		{#each hits as hit (hit.item.id)}
			{@const value = valueOf(hit)}
			<!-- The combobox input carries the keys (arrows and Enter); the options are pointer targets. -->
			<!-- svelte-ignore a11y_click_events_have_key_events -->
			<li
				id="settings-search-{hit.item.id}"
				role="option"
				aria-selected={activeId === hit.item.id}
				class="search-option"
				data-active={activeId === hit.item.id || undefined}
				data-testid="settings-search-result"
				onpointermove={() => onactive(hit.item.id)}
				onpointerdown={(e) => e.preventDefault()}
				onclick={() => onpick(hit)}
			>
				<span class="font-medium">{hit.item.label}</span>
				<span class="search-crumb" data-testid="settings-search-crumb">{hit.item.crumb}</span>
				{#if value}
					<span class="search-value">
						{#if value.mono}<Mono>{value.text}</Mono>{:else}{value.text}{/if}
						{#if value.source}<span class="search-source">· {value.source}</span>{/if}
					</span>
				{/if}
			</li>
		{/each}
	</ul>
{/if}

<style>
	.search-list {
		display: flex;
		flex-direction: column;
		max-height: var(--listbox-max-height);
		overflow-y: auto;
		border: var(--border) solid var(--color-base-300);
		border-radius: var(--radius-field);
		background: var(--color-base-100);
	}
	.search-option {
		display: flex;
		flex-direction: column;
		gap: calc(var(--spacing) * 0.5);
		min-height: var(--control-height);
		padding: calc(var(--spacing) * 2) calc(var(--spacing) * 3);
		cursor: pointer;
	}
	@media (pointer: coarse) {
		.search-option {
			min-height: var(--target-min);
		}
	}
	.search-option[data-active] {
		background: var(--color-base-200);
	}
	.search-crumb,
	.search-value {
		font-size: var(--text-small);
		color: var(--color-muted-text);
	}
	.search-none {
		display: flex;
		flex-direction: column;
		gap: calc(var(--spacing) * 2);
		padding: calc(var(--spacing) * 3);
		border: var(--border) solid var(--color-base-300);
		border-radius: var(--radius-field);
	}
	.search-none a {
		text-decoration: underline;
		color: var(--color-primary);
	}
</style>
