<script lang="ts" module>
	export interface ConfirmAction {
		id: string;
		label: string;
		variant?: 'primary' | 'secondary' | 'danger';
	}
</script>

<script lang="ts">
	// One modal question with a few answers (§10.10.5's leave dialog, §10.10.7's confirmations).
	// Opened through `ask()` (dialogs.ts), so it loads only when a question is asked.
	import Button from '$lib/ui/controls/Button.svelte';
	import Dialog from '$lib/ui/overlays/Dialog.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { tick } from 'svelte';

	interface Props {
		title: string;
		/** Sentences under the title. */
		body: string[];
		/** In footer order; the last is the main action. */
		actions: ConfirmAction[];
		/** The answer focused first (§10.10.7: Cancel for a destructive one). */
		initial?: string;
		/** Called once with the chosen id, or null when closed with Esc or ✕. */
		onclose: (id: string | null) => void;
	}

	let { title, body, actions, initial, onclose }: Props = $props();

	let open = $state(true);
	let answered = false;
	const uid = $props.id();

	function answer(id: string | null) {
		if (answered) return;
		answered = true;
		open = false;
		onclose(id);
	}

	$effect(() => {
		// The dialog focuses its first control (✕); move to the chosen one once it's open.
		const id = initial ?? actions.at(-1)?.id;
		void tick().then(() =>
			requestAnimationFrame(() =>
				document.querySelector<HTMLElement>(`[data-confirm="${uid}:${id}"]`)?.focus()
			)
		);
	});
</script>

<Dialog
	bind:open
	{title}
	closeLabel={m.settings_form_dialog_close()}
	onOpenChange={(o) => {
		if (!o) answer(null);
	}}
>
	<div class="flex flex-col gap-2">
		{#each body as p, i (i)}
			<p>{p}</p>
		{/each}
	</div>
	{#snippet footer()}
		{#each actions as a (a.id)}
			<Button
				variant={a.variant ?? 'secondary'}
				data-confirm="{uid}:{a.id}"
				onclick={() => answer(a.id)}>{a.label}</Button
			>
		{/each}
	{/snippet}
</Dialog>
