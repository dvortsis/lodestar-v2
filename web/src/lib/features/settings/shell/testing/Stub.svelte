<script lang="ts">
	// A stand-in body for registry tests: a hand-built screen's heading, an extra or the form.
	let { text = 'Stub', heading = false }: { text?: string; heading?: boolean } = $props();
</script>

{#if heading}<h1>{text}</h1>{:else}<p data-testid="stub">{text}</p>{/if}
