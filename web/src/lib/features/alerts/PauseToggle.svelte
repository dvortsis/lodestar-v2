<script lang="ts">
	// Pausing is a toggle with no confirmation (§10.8). It sends PATCH {paused}, shows pending
	// (`aria-busy`, no second send) until the answer, then the server's value. On an error it goes
	// back to what it was, with a toast.
	import Toggle from '$lib/ui/fields/Toggle.svelte';
	import { toast } from '$lib/ui/feedback/toasts.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { setPaused, type SavedSearch } from './api';

	let { search, onUpdated }: { search: SavedSearch; onUpdated: (updated: SavedSearch) => void } =
		$props();

	// What the switch shows: the server's value, except while an answer is awaited.
	let shown = $state(false);
	let pending = $state(false);
	let resumed = $state(false);
	$effect.pre(() => {
		if (!pending) shown = search.paused;
	});

	async function change() {
		if (pending) {
			shown = search.paused; // a second flip while waiting is ignored
			return;
		}
		const wanted = shown;
		pending = true;
		try {
			const updated = await setPaused(search.id, wanted);
			shown = updated.paused;
			resumed = !updated.paused;
			pending = false;
			onUpdated(updated);
		} catch {
			pending = false;
			shown = search.paused;
			resumed = false;
			toast.push({ kind: 'error', text: m.alerts_pause_failed({ name: search.name }) });
		}
	}
</script>

<span class="al-pause">
	<Toggle
		label={m.alerts_pause_label()}
		aria-label={m.alerts_pause_aria({ name: search.name })}
		bind:checked={shown}
		aria-busy={pending || undefined}
		onchange={change}
		data-testid="alerts-pause-{search.id}"
	/>
	<!-- The rule, stated once after a resume. -->
	<span class="al-help" role="status" data-testid="alerts-resume-help-{search.id}">
		{#if resumed && !shown}{m.alerts_resume_help()}{/if}
	</span>
</span>

<style>
	.al-pause {
		display: inline-flex;
		flex-direction: column;
		gap: calc(var(--spacing) * 1);
	}
	.al-help {
		color: var(--color-muted-text);
		font-size: var(--text-small);
		line-height: var(--text-small--line-height);
	}
</style>
