<script lang="ts">
	// The keyboard shortcut sheet (§10.16): every command that has a key, grouped by section, the
	// ones on this screen first. Opened by `?` through boot.ts; it stays mounted once loaded.
	import { resolve } from '$app/paths';
	import { keys, type CommandInfo } from '$lib/keys';
	import { isSingleKey } from '$lib/keys/combo';
	import { m } from '$lib/paraglide/messages.js';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import Dialog from '$lib/ui/overlays/Dialog.svelte';
	import { toggleSingleKeys } from './toggle';
	import Keycap from './Keycap.svelte';
	import { sheet } from './sheet.svelte';
	import type { Route } from '$lib/shell/nav';
	import './shortcuts.css';

	const withKeys = $derived(keys.list().filter((c) => c.keys.length > 0));
	const titleOf = (id: string) => keys.list().find((c) => c.id === id)?.title ?? id;

	function group(list: CommandInfo[]) {
		const by = new Map<string, CommandInfo[]>(); // eslint-disable-line svelte/prefer-svelte-reactivity -- a local lookup
		for (const c of list) by.set(c.section, [...(by.get(c.section) ?? []), c]);
		return [...by].map(([section, items]) => ({ section, items }));
	}
	const here = $derived(group(withKeys.filter((c) => c.active)));
	const elsewhere = $derived(withKeys.filter((c) => !c.active));
	const off = (c: CommandInfo) => !keys.singleKeys && c.keys.every(isSingleKey);
	const href = resolve('/settings/keyboard' as Route);
</script>

<Dialog
	bind:open={sheet.open}
	title={m.shortcuts_sheet_title()}
	closeLabel={m.shortcuts_sheet_close()}
>
	<div class="sc-sheet" data-testid="shortcut-sheet">
		{#if !keys.singleKeys}
			<Banner kind="info">
				{m.shortcuts_sheet_off_notice()}
				{#snippet action()}
					<Button dense onclick={() => void toggleSingleKeys()}
						>{m.shortcuts_sheet_turn_on()}</Button
					>
				{/snippet}
			</Banner>
		{/if}
		{#each keys.conflicts() as c (c.scope + c.combo)}
			<Banner kind="warning">
				{m.shortcuts_sheet_conflict({
					first: titleOf(c.ids[0]!),
					second: c.ids.slice(1).map(titleOf).join(', '),
					key: c.combo
				})}
			</Banner>
		{/each}
		{#snippet rows(items: CommandInfo[])}
			<ul class="sc-list">
				{#each items as c (c.id)}
					<li class="sc-item">
						<span>{c.title}</span>
						<span class="sc-keys">
							{#each c.keys as combo (combo)}<Keycap {combo} />{/each}
							{#if off(c)}<span class="sc-tag">{m.shortcuts_sheet_off_tag()}</span>{/if}
						</span>
					</li>
				{/each}
			</ul>
		{/snippet}
		{#each here as g (g.section)}
			<section>
				<h3 class="sc-heading">{g.section}</h3>
				{@render rows(g.items)}
			</section>
		{/each}
		{#if elsewhere.length}
			<section>
				<h3 class="sc-heading">{m.shortcuts_sheet_other()}</h3>
				{@render rows(elsewhere)}
			</section>
		{/if}
		{#if !withKeys.length}<p class="sc-muted">{m.shortcuts_sheet_empty()}</p>{/if}
	</div>
	{#snippet footer()}
		<!-- Provisional: /settings/keyboard answers 404 until S-settings-1's route finds the screen. -->
		<a {href} class="sc-link" onclick={() => (sheet.open = false)}>{m.shortcuts_sheet_change()}</a>
	{/snippet}
</Dialog>
