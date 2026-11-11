<script lang="ts">
	// What "Set up" starts: the server's offer for the companion, then Settings → Companions' own
	// install dialog, in place. Loaded on first use, so Sources carries none of it until then. The
	// request carries only the offer's version and SHA-256; the server decides.
	import { onDestroy, onMount } from 'svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import { failureWords, type InstallOffer } from '../settings/companions/logic';
	import { CompanionsStore } from '../settings/companions/store.svelte';
	import InstallDialog from '../settings/companions/InstallDialog.svelte';

	interface Props {
		companion: string;
		/** The offer arrived or was refused: Set up stops showing it is working. */
		onsettled: () => void;
		/** The dialog is gone (cancelled, or installed). */
		onclose: () => void;
		/** The install succeeded: the screen reads the source again, so no stale "Needs X" stays. */
		oninstalled: () => void | Promise<void>;
	}
	let { companion, onsettled, onclose, oninstalled }: Props = $props();

	const store = new CompanionsStore();
	onDestroy(() => store.stop());

	let offer = $state<InstallOffer | null>(null);
	let busy = $state(false);
	let error = $state('');
	let dialogError = $state('');
	/** The dialog was closed (Esc, Cancel) while its request was still running. */
	let closedWhileBusy = false;

	onMount(async () => {
		try {
			offer = await store.offer(companion);
		} catch (e) {
			error = failureWords(e);
		}
		onsettled();
	});

	async function confirm() {
		if (!offer || busy) return;
		busy = true;
		dialogError = '';
		closedWhileBusy = false;
		try {
			await store.install(offer);
			offer = null;
			onclose();
			await oninstalled();
		} catch (e) {
			dialogError = failureWords(e);
			// A closed dialog shows nothing: the chip says it instead.
			if (closedWhileBusy) error = dialogError;
		} finally {
			busy = false;
		}
	}
</script>

{#if error}<Banner kind="error">{error}</Banner>{/if}
{#key offer}
	{#if offer}
		<InstallDialog
			{offer}
			{busy}
			error={dialogError}
			onconfirm={() => void confirm()}
			onclose={() => {
				closedWhileBusy = busy;
				const was = offer;
				setTimeout(() => {
					if (was === offer) {
						offer = null;
						onclose();
					}
				}, 400);
			}}
		/>
	{/if}
{/key}
