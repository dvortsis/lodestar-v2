<script lang="ts">
	// Loads the Legal Notice and shows it, with a loading line and a retry when it can't be
	// fetched. `notice` is bound so the form around it knows which version it showed.
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import LegalNotice from './LegalNotice.svelte';
	import { fontsSettled, loadNotice, type LegalNotice as Notice } from './notice';

	interface Props {
		notice?: Notice | null;
		scroll?: boolean;
	}

	let { notice = $bindable(null), scroll = true }: Props = $props();
	let failed = $state(false);
	let loading = false;

	async function load() {
		failed = false;
		loading = true;
		try {
			// Shown in its final font, so nothing reflows after it appears.
			const [loaded] = await Promise.all([loadNotice(), fontsSettled()]);
			notice = loaded;
		} catch {
			failed = true;
		} finally {
			loading = false;
		}
	}

	// On mount, and again whenever the form drops a stale notice.
	$effect(() => {
		if (!notice && !failed && !loading) void load();
	});
</script>

{#if notice}
	<LegalNotice text={notice.text} label={m.auth_notice_label()} {scroll} />
{:else if failed}
	<Banner kind="error">
		{m.auth_notice_failed()}
		{#snippet action()}
			<Button variant="secondary" onclick={load}>{m.auth_notice_retry()}</Button>
		{/snippet}
	</Banner>
{:else}
	<!-- In a box, the loading line holds the box's height, so the notice arriving moves nothing
	     (the notice is always taller than the box). -->
	<p
		role="status"
		class={[
			'text-body text-muted-text',
			scroll && 'h-(--notice-max-height) rounded-box border border-base-300 p-4'
		]}
	>
		{m.auth_notice_loading()}
	</p>
{/if}
