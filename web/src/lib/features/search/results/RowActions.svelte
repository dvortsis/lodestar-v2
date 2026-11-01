<script module lang="ts">
	import { results } from './results.svelte';
	// A rule was added by "Block this…" in the drawer, which fires `ls-stale` (the row's own dialog reloads when it closes): refetch, so
	// a hidden torrent drops. Not on /t, where no list is mounted.
	addEventListener('ls-stale', () => location.pathname.startsWith('/t/') || results.reload());
</script>

<script lang="ts">
	// Search's row actions (§10.4): Copy magnet (icon + label in Comfortable) and More. They show on
	// hover and focus, and always under a coarse pointer (row.css). Loaded with the first results,
	// never by Discover or Alerts. Out of the tab order: the list is one tab stop with roving focus.
	// The keyboard has `c` and `o`, and Shift+F10 or the menu key on a row opens More (the panel).
	// "Send to <client>" joins the menu once a download client is known (read on the first More).
	import { tick } from 'svelte';
	import Copy from '~icons/ph/copy-bold';
	import Dots from '~icons/ph/dots-three-bold';
	import { m } from '$lib/paraglide/messages.js';
	import { toast } from '$lib/ui/feedback/toasts.svelte';
	import { copyText } from '$lib/util/clipboard';
	import { torrentTabHref } from './links';
	import type { Density, SearchHit } from './ResultRow.svelte';

	let { hit, density, testid }: { hit: SearchHit; density: Density; testid: string } = $props();

	const name = $derived(hit.title ?? hit.name);

	// The menu (Bits UI, its popper and portal) loads on the first click of More, not with the
	// results: it opens as it mounts, on the button that replaced the placeholder.
	let Menu = $state<typeof import('$lib/ui/overlays/Menu.svelte').default>();
	let open = $state(false);
	let sendLabel = $state<string | null>(null);
	function more(event: MouseEvent) {
		event.stopPropagation();
		void import('$lib/ui/overlays/Menu.svelte').then((mod) => {
			Menu = mod.default;
			open = true;
		});
		// The Send item joins when the client's name arrives; the menu doesn't wait for it.
		void import('$lib/features/detail/send')
			.then((s) => s.sendLabel())
			.then(
				(label) => (sendLabel = label),
				() => {}
			);
	}

	function copy(event: MouseEvent) {
		event.stopPropagation(); // not the row's activation (it would open the drawer)
		void import('$lib/features/detail/magnet').then(
			(mod) => mod.copyMagnet({ hash: hit.hash }),
			() => toast.push({ kind: 'error', text: m.search_results_error_body() })
		);
	}

	// "Block this…" (§20.2): the flows' dialog, through their one door (S-filters-2c). It only ever
	// adds a rule, previewed first, for this row's hash and the name the row shows.
	let Block = $state.raw<typeof import('$lib/features/filters/flows-root').BlockThisDialog>();
	let blocking = $state(false);
	// Refetch only once the dialog closes: the hidden row (and this dialog) goes with the reload.
	let applied = false;
	// Keep the place: reload as many rows as were shown, then focus the row that took this one's
	// slot (the next, or the previous if it was last), or the results heading when none is left.
	async function settle() {
		const at = results.hits.findIndex((h) => h.hash === hit.hash);
		const had = results.hits.length;
		await results.reload(had);
		const rows = results.hits;
		const gone = had - rows.length;
		results.note = gone > 0 ? m.search_results_blocked({ count: gone }) : '';
		await tick();
		const next = rows[Math.min(Math.max(at, 0), rows.length - 1)];
		const row = next && document.querySelector(`[data-hash="${next.hash}"]`);
		(row?.closest<HTMLElement>('[data-index]') ?? document.getElementById('rp-label'))?.focus({
			preventScroll: true
		});
	}

	async function select(id: string) {
		if (id === 'block')
			void import('$lib/features/filters/flows-door')
				.then((door) => door.load())
				.then(
					(mod) => ((Block = mod.BlockThisDialog), (blocking = true)),
					() => toast.push({ kind: 'error', text: m.search_results_error_body() })
				);
		if (id === 'send')
			void import('$lib/features/detail/send').then(
				(mod) => mod.sendHash(hit.hash),
				() => toast.push({ kind: 'error', text: m.search_results_error_body() })
			);
		if (id === 'open') window.open(torrentTabHref(hit.hash), '_blank', 'noopener');
		if (id === 'hash') {
			// The Clipboard API, else the textarea fallback (plain-HTTP LAN origins have no API).
			const copied = (await copyText(hit.hash)) === 'copied';
			toast.push(
				copied
					? { kind: 'success', text: m.search_results_hash_copied() }
					: { kind: 'error', text: m.search_results_copy_failed() }
			);
		}
	}
</script>

<button
	type="button"
	class="rr-action"
	tabindex="-1"
	aria-label={m.search_results_copy()}
	data-testid="{testid}-copy"
	onclick={copy}
>
	<Copy class="rr-icon" aria-hidden="true" />
	{#if density === 'comfortable'}<span class="rr-action-label">{m.search_results_copy()}</span>{/if}
</button>
{#snippet moreButton(props: Record<string, unknown>, onclick: (e: MouseEvent) => void)}
	<button
		type="button"
		class="rr-action"
		tabindex="-1"
		aria-label={m.search_results_more({ name })}
		data-testid="{testid}-more"
		data-row-more
		aria-keyshortcuts="Shift+F10"
		{...props}
		{onclick}
	>
		<Dots class="rr-icon" aria-hidden="true" />
	</button>
{/snippet}
{#if Menu}
	<Menu
		bind:open
		label={m.search_results_more({ name })}
		items={[
			...(sendLabel ? [{ id: 'send', label: sendLabel }] : []),
			{ id: 'open', label: m.search_results_open_tab() },
			{ id: 'hash', label: m.search_results_copy_hash() },
			{ id: 'block', label: m.search_results_block() }
		]}
		onSelect={(id) => void select(id)}
	>
		{#snippet trigger({ props })}
			{@render moreButton(props, (e) => {
				e.stopPropagation(); // not the row's activation
				(props.onclick as ((e: MouseEvent) => void) | undefined)?.(e);
			})}
		{/snippet}
	</Menu>
{:else}
	{@render moreButton({ 'aria-haspopup': 'menu', 'aria-expanded': 'false' }, more)}
{/if}
{#if Block}
	<Block
		torrent={{ hash: hit.hash, hash_v2: hit.hash_v2, name: hit.name }}
		bind:open={blocking}
		onapplied={() => (applied = true)}
		onclose={() => applied && void settle()}
	/>
{/if}
