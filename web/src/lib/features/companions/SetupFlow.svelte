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
		/** True while the offer is read or the install runs: Set up shows it is working until then. */
		onworking: (working: boolean) => void;
		/** The dialog is gone (cancelled, or installed). Never while an install is running. */
		onclose: () => void;
		/** The install succeeded: the screen reads the source again, so no stale "Needs X" stays. */
		oninstalled: () => void | Promise<void>;
	}
	let { companion, onworking, onclose, oninstalled }: Props = $props();

	const store = new CompanionsStore();
	onDestroy(() => store.stop());

	let offer = $state<InstallOffer | null>(null);
	let busy = $state(false);
	let error = $state('');
	let dialogError = $state('');
	/** The dialog was closed (Esc, Cancel) while its request was still running. */
	let closedWhileBusy = false;
	let timer: ReturnType<typeof setTimeout> | undefined;
	onDestroy(() => clearTimeout(timer));

	onMount(async () => {
		try {
			offer = await store.offer(companion);
		} catch (e) {
			error = failureWords(e);
		}
		onworking(false);
	});

	async function confirm() {
		if (!offer || busy) return;
		busy = true;
		onworking(true);
		dialogError = '';
		closedWhileBusy = false;
		let installed = false;
		try {
			await store.install(offer);
			installed = true;
		} catch (e) {
			dialogError = failureWords(e);
			// A closed dialog shows nothing: this says it instead, and stays until the next attempt.
			if (closedWhileBusy) {
				error = dialogError;
				offer = null;
			}
		} finally {
			busy = false;
			onworking(false);
		}
		if (installed) {
			offer = null;
			onclose();
			await oninstalled();
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
				// An install that is running keeps this mounted: its answer (or refusal) still has to
				// be shown. Otherwise the dialog goes after its exit animation.
				const was = offer;
				timer = setTimeout(() => {
					if (was === offer && !busy) {
						offer = null;
						onclose();
					}
				}, 400);
			}}
		/>
	{/if}
{/key}
