<script lang="ts">
	// The header's one primary action: Send to the download client once one is known (send.ts, read
	// lazily after mount), else Copy magnet, which then stays as the secondary action.
	import { onDestroy } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import IconDots from '~icons/ph/dots-three-bold';
	import Button from '$lib/ui/controls/Button.svelte';
	import { isMagnet } from '$lib/util/magnet';
	import { detailGeneration, onDetailInvalidated, type TorrentDetail } from './cache';

	interface Props {
		detail: TorrentDetail | null;
		/** The drawer adds "Open full page" to the menu; the page it opens has no such item. */
		mode?: 'drawer' | 'page';
	}
	let { detail, mode = 'page' }: Props = $props();
	const magnet = $derived(detail && isMagnet(detail.magnet) ? detail.magnet : null);
	let gone = false;
	onDestroy(() => (gone = true));
	async function copy() {
		const value = magnet;
		const generation = detailGeneration();
		if (!value) return;
		const { copyMagnet } = await import('./magnet');
		if (!gone && generation === detailGeneration() && value === magnet)
			await copyMagnet({ magnet: value });
	}
	let label = $state<string | null>(null);
	let go: (hash: string) => unknown = () => {};
	// With no download client the More actions menu gets a last item, "Set up a download client…"
	// (nothing appears after paint, so the layout never shifts). An unreadable read (offline, signed
	// out) isn't "no client", so it adds nothing.
	let noClient = $state(false);
	void import('./send').then(
		(s) =>
			s.sendLabel(true).then((l) => {
				go = s.sendHash;
				label = l;
				noClient = !l && !gone && !s.clientUnreadable();
			}),
		() => {}
	);

	// The menu (bits-ui) loads on the first press; it opens as soon as it has.
	let ActionsMenu = $state.raw<typeof import('./ActionsMenu.svelte').default>();
	let menuOpen = $state(false);
	let menuAsked = false;
	function openMenu() {
		menuOpen = true;
		if (menuAsked) return;
		menuAsked = true;
		void import('./ActionsMenu.svelte').then(
			(mod) => (ActionsMenu = mod.default),
			() => {
				menuAsked = false;
				menuOpen = false;
				void import('./commands').then((c) => c.failed(true));
			}
		);
	}

	// ArrowDown and ArrowUp open the menu from the keyboard before its chunk has loaded.
	function openOnArrow(event: KeyboardEvent) {
		if (event.key !== 'ArrowDown' && event.key !== 'ArrowUp') return;
		event.preventDefault();
		openMenu();
	}

	// "Block this…" (§20.2) through the flows' one door (S-filters-2c): it only ever adds a rule,
	// previewed first, for the torrent shown when it was pressed (its hash and name), and only
	// while that torrent is still the one shown.
	type Target = { hash: string; hash_v2: string | null; name: string };
	let Block = $state.raw<typeof import('$lib/features/filters/flows-root').BlockThisDialog>();
	let target = $state.raw<Target>();
	let blocking = $state(false);
	// Visibility changed (e.g. "Show adult content" turned off): the dialog goes with what it showed.
	onDestroy(onDetailInvalidated(() => (target = undefined)));
	async function block() {
		if (!detail) return;
		const { hash, hash_v2, name } = detail;
		const generation = detailGeneration();
		const mod = await import('$lib/features/filters/flows-door')
			.then((door) => door.load())
			.catch(() => (void import('./commands').then((c) => c.failed(true)), null));
		if (!mod) return; // offline or a stale build: the button stays, a press tries again
		if (gone || generation !== detailGeneration() || hash !== detail?.hash) return;
		Block = mod.BlockThisDialog;
		target = { hash, hash_v2, name };
		blocking = true;
	}
</script>

{#if label && detail}
	<Button variant="primary" dense data-testid="detail-send" onclick={() => go(detail.hash)}
		>{label}</Button
	>
{/if}
<Button
	variant={label ? undefined : 'primary'}
	dense
	disabled={!magnet}
	data-testid="detail-copy-magnet"
	onclick={copy}
>
	{m.detail_copy_magnet()}
</Button>
{#if magnet}
	<!-- eslint-disable svelte/no-navigation-without-resolve -- a magnet URI, checked by isMagnet -->
	<a
		class="btn btn-sm btn-outline border-field-border"
		href={magnet}
		data-testid="detail-open-magnet">{m.detail_open_magnet()}</a
	>
	<!-- eslint-enable svelte/no-navigation-without-resolve -->
{/if}
<!-- Shown once there is a torrent, never disabled first: an outline button fading from its
     disabled colours reads as low contrast mid-transition (axe, rose-pine-dark). -->
{#if detail}
	{#if ActionsMenu}
		<ActionsMenu bind:open={menuOpen} {mode} hash={detail.hash} onblock={block} {noClient} />
	{:else}
		<Button
			dense
			aria-haspopup="menu"
			aria-expanded="false"
			aria-label={m.detail_actions_menu()}
			data-testid="detail-actions-menu"
			onclick={openMenu}
			onkeydown={openOnArrow}
		>
			<IconDots class="size-(--icon-size-nav)" aria-hidden="true" />
		</Button>
	{/if}
{/if}
{#if Block && target && target.hash === detail?.hash}
	<Block
		torrent={target}
		bind:open={blocking}
		onapplied={() => dispatchEvent(new Event('ls-stale'))}
	/>
{/if}
