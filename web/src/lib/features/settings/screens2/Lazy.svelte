<script lang="ts">
	// A part of a connection screen that isn't needed to paint its first fields (a test, a dialog,
	// the client cards), loaded once it's on the page. Until then nothing shows, and a chunk that
	// fails to load says so in its place (browsers keep a failed chunk, so there is no retry).
	import type { Component } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';

	interface Props {
		load: () => Promise<{ default: Component<any> }>; // eslint-disable-line @typescript-eslint/no-explicit-any
		props?: Record<string, unknown>;
	}
	let { load, props = {} }: Props = $props();

	let Part = $state<Component<any>>(); // eslint-disable-line @typescript-eslint/no-explicit-any
	let failed = $state(false);
	$effect(() => {
		let live = true;
		void load().then(
			(mod) => {
				if (live) Part = mod.default;
			},
			() => {
				if (live) failed = true;
			}
		);
		return () => {
			live = false;
		};
	});
</script>

{#if Part}
	<Part {...props} />
{:else if failed}
	<p role="alert" class="text-small text-error">{m.settings_form_chunk_failed()}</p>
{/if}
