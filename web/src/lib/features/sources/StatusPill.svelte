<script lang="ts">
	// A source's status: always an icon and its text, never colour alone (§10.2).
	import IconOff from '~icons/ph/minus-circle';
	import { statusIcons, statusInk } from '$lib/ui/status';
	import { STATUS_VARIANT, statusText, type Status } from './sources';

	let { status, testid }: { status: Status; testid: string } = $props();
	const variant = $derived(STATUS_VARIANT[status]);
	const Icon = $derived(variant === 'neutral' ? IconOff : statusIcons[variant]);
	const ink = $derived(variant === 'neutral' ? 'text-muted-text' : statusInk[variant]);
</script>

<span class="inline-flex items-center gap-1 text-caption" data-testid={testid} data-status={status}>
	<span class={ink} aria-hidden="true"><Icon /></span>
	<span>{statusText(status)}</span>
</span>
