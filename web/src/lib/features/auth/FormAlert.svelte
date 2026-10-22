<script lang="ts">
	// The form-level message: a failure that belongs to no field, the rate-limit countdown, and
	// "you can try again" once the lock lifts. The countdown's digits are hidden from screen
	// readers (they'd be read every second); the wait is announced once, when it starts.
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import type { AuthForm } from './form.svelte';
	import { failureMessage } from './errors';

	interface Props {
		form: AuthForm;
	}

	let { form }: Props = $props();

	let lockedFor = $state(0);
	let unlocked = $state(false);
	let wasLocked = false;

	$effect.pre(() => {
		const locked = form.state.name === 'locked';
		if (locked && !wasLocked) lockedFor = form.secondsLeft;
		if (locked) unlocked = false;
		else if (wasLocked) unlocked = form.state.name === 'idle';
		if (form.state.name === 'submitting') unlocked = false;
		wasLocked = locked;
	});

	const failure = $derived(
		form.state.name === 'error' && !form.state.failure.field ? form.state.failure : null
	);
</script>

<div aria-live="polite" class="contents">
	{#if unlocked}
		<Banner kind="info">{m.auth_error_unlocked()}</Banner>
	{/if}
</div>
{#if form.state.name === 'locked'}
	<Banner kind="error" title={m.auth_error_rate_limited()}>
		<span aria-hidden="true" data-testid="auth-countdown"
			>{m.auth_error_rate_limited_countdown({ seconds: String(form.secondsLeft) })}</span
		>
		<span class="sr-only"
			>{m.auth_error_rate_limited_countdown({ seconds: String(lockedFor) })}</span
		>
	</Banner>
{:else if failure}
	<Banner kind="error">{failureMessage(failure)}</Banner>
{/if}
