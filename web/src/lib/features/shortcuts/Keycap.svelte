<script lang="ts">
	// A shortcut in Kbd, with a name for screen readers (`/` is "Slash").
	import Kbd from '$lib/ui/labels/Kbd.svelte';
	import { shown, keyNameFor } from './keyname';

	let { combo, mac }: { combo: string; mac?: boolean } = $props();
</script>

<Kbd
	><span aria-hidden="true">{shown(combo, mac)}</span><span class="sr-only"
		>{keyNameFor(combo, mac)}</span
	></Kbd
>
