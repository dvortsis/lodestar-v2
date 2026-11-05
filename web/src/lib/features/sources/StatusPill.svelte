<script lang="ts">
	// A source's status: always an icon and its text, never colour alone (§10.2).
	import { statusIcons, statusInk } from '$lib/ui/status';
	import { STATUS_VARIANT, statusText, type Status } from './sources';

	let { status, testid }: { status: Status; testid: string } = $props();
	const variant = $derived(STATUS_VARIANT[status]);
	const Icon = $derived(variant === 'neutral' ? null : statusIcons[variant]);
	const ink = $derived(variant === 'neutral' ? 'text-muted-text' : statusInk[variant]);
</script>

<span class="inline-flex items-center gap-1 text-caption" data-testid={testid} data-status={status}>
	<span class={ink} aria-hidden="true"
		>{#if Icon}<Icon
			/>{:else}<!-- Off: a minus in a circle, inline so no shared icon chunk moves. -->
			<svg viewBox="0 0 256 256" width="1.2em" height="1.2em" fill="currentColor"
				><path
					d="M176 128a8 8 0 0 1-8 8H88a8 8 0 0 1 0-16h80a8 8 0 0 1 8 8m56 0A104 104 0 1 1 128 24a104.11 104.11 0 0 1 104 104m-16 0a88 88 0 1 0-88 88a88.1 88.1 0 0 0 88-88"
				/></svg
			>{/if}</span
	>
	<span>{statusText(status)}</span>
</span>
