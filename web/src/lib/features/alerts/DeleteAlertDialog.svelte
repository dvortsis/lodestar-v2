<script lang="ts">
	// Deleting asks a specific confirmation (§10.8): the search's name, and what goes with it.
	// Loaded with `import()` on first use, like the save dialog.
	import Dialog from '$lib/ui/overlays/Dialog.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import { ApiError } from '$lib/api/client';
	import { m } from '$lib/paraglide/messages.js';
	import { deleteSavedSearch, type SavedSearch } from './api';
	import { focusAfterOpen } from './focus';

	interface Props {
		open?: boolean;
		search: Pick<SavedSearch, 'id' | 'name' | 'rss'>;
		/** Called once the search is gone (a 404 counts: it is). */
		onDeleted: (search: Pick<SavedSearch, 'id' | 'name' | 'rss'>) => void;
	}
	let { open = $bindable(false), search, onDeleted }: Props = $props();

	let busy = $state(false);
	let failed = $state(false);

	$effect(() => {
		if (open) failed = false;
	});

	// The dialog opens on Cancel, the safe choice. Bits focuses the first control when it mounts,
	// so wait for that.
	const cancelId = $props.id();
	$effect(() => {
		if (!open) return;
		return focusAfterOpen(() => document.getElementById(cancelId));
	});

	async function confirm() {
		if (busy) return;
		busy = true;
		failed = false;
		try {
			await deleteSavedSearch(search.id);
		} catch (e) {
			if (!(e instanceof ApiError && e.code === 'saved_search_not_found')) {
				failed = true;
				busy = false;
				return;
			}
		}
		busy = false;
		open = false;
		onDeleted(search);
	}
</script>

<Dialog
	bind:open
	title={m.alerts_delete_title({ name: search.name })}
	description={search.rss ? m.alerts_delete_body_rss() : m.alerts_delete_body()}
	closeLabel={m.alerts_close()}
>
	{#if failed}
		<Banner kind="error">{m.alerts_delete_failed({ name: search.name })}</Banner>
	{/if}
	{#snippet footer()}
		<Button id={cancelId} onclick={() => (open = false)} data-testid="alerts-delete-cancel">
			{m.alerts_cancel()}
		</Button>
		<Button variant="danger" loading={busy} onclick={confirm} data-testid="alerts-delete-confirm">
			{m.alerts_delete()}
		</Button>
	{/snippet}
</Dialog>
