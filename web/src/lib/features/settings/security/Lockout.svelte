<script lang="ts">
	// The login bucket's lock-out (429, F-auth-lock), shown truthfully: how long to wait, then that
	// it's over. The bucket counts every try, right or wrong, so it says what M-auth's sign-in says,
	// not that the password was wrong. The screen holds its button on its own timer.
	import { untrack } from 'svelte';
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Banner from '$lib/ui/feedback/Banner.svelte';

	interface Props {
		/** The wait in seconds, as the refusal gave it; a new refusal is a new key. */
		seconds: number;
	}
	let { seconds }: Props = $props();

	const until = Date.now() + untrack(() => seconds) * 1000;
	let clock = $state(Date.now());
	const left = $derived(Math.max(0, Math.ceil((until - clock) / 1000)));

	$effect(() => {
		const t = setInterval(() => (clock = Date.now()), 250);
		return () => clearInterval(t);
	});
</script>

{#if left > 0}
	<Banner kind="error" title={m.auth_error_rate_limited()}>
		<span aria-hidden="true" data-testid="lockout-countdown"
			>{m.settings_security_confirm_wait({ seconds: formatCount(left, currentLocale()) })}</span
		>
		<!-- The moving digits are hidden; the wait is read once, as it began. -->
		<span class="sr-only"
			>{m.settings_security_confirm_wait({ seconds: formatCount(seconds, currentLocale()) })}</span
		>
	</Banner>
{:else}
	<Banner kind="info">{m.settings_security_confirm_unlocked()}</Banner>
{/if}
