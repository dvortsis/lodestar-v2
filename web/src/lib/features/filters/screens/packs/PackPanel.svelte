<script lang="ts">
	// One pack: its version, the pending versions with "What changed", the corpus status and its
	// rules. Everything shown is the server's last answer.
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Badge from '$lib/ui/labels/Badge.svelte';
	import Mono from '$lib/ui/labels/Mono.svelte';
	import type { Ask } from '../types';
	import { acceptVersions } from './ops';
	import { directionText } from './text';
	import RuleRow from './RuleRow.svelte';
	import type { PackCorpus, PackState, PendingPackVersion } from './types';

	interface Props {
		pack: PackState;
		pending: PendingPackVersion[];
		/** Null while loading or when it couldn't be read. */
		corpus: PackCorpus | null;
		ask: Ask;
	}
	let { pack, pending, corpus, ask }: Props = $props();

	const mine = $derived(corpus?.rules.filter((c) => c.pack === pack.id) ?? []);
	const failing = $derived(mine.reduce((n, c) => n + c.failing, 0));
	const gaps = $derived(mine.reduce((n, c) => n + c.gaps, 0));
	const disagree = $derived(mine.reduce((n, c) => n + c.direction_disagreements, 0));
</script>

<section
	class="flex flex-col gap-3"
	aria-labelledby="pack-h-{pack.id}"
	data-testid="pack-panel-{pack.id}"
>
	<h3 id="pack-h-{pack.id}" class="text-small font-semibold">
		<Mono>{pack.id}</Mono>
	</h3>
	<p class="text-muted-text">
		{m.filters_packs_version()}
		<Mono wrap>{pack.version}</Mono>
	</p>

	{#if pending.length}
		<div
			class="flex flex-col gap-3 rounded-box border border-info p-3"
			data-testid="pack-pending-{pack.id}"
		>
			<Badge variant="info">{m.filters_packs_pending_title()}</Badge>
			<p class="text-muted-text">{m.filters_packs_pending_help()}</p>
			<ul class="flex flex-col gap-3">
				{#each pending as p (p.rule)}
					<li class="flex flex-col gap-2" data-testid="pack-pending-{p.rule}">
						<div class="flex flex-wrap items-center gap-3">
							<Mono>{p.rule}</Mono>
							<span
								>{p.applied === null
									? m.filters_packs_pending_new({ version: p.version })
									: m.filters_packs_pending_from({ from: p.applied, to: p.version })}</span
							>
							<Badge>{directionText(p.direction)}</Badge>
							<Button
								dense
								onclick={() =>
									ask(acceptVersions([p.rule]), {
										title: m.filters_packs_accept_title({ id: p.rule }),
										applyLabel: m.filters_packs_accept_apply({ id: p.rule })
									})}
								data-testid="pack-accept-{p.rule}">{m.filters_packs_accept({ id: p.rule })}</Button
							>
						</div>
						<ul class="flex flex-col gap-1 text-small">
							{#each p.changelog as c (c.version)}
								<li>
									<span class="font-semibold">@{c.version}</span> · {directionText(c.direction)}
									<!-- Text, never HTML: a changelog is shown as the words it holds. -->
									<span class="whitespace-pre-wrap text-muted-text">{c.text ?? ''}</span>
								</li>
							{/each}
						</ul>
					</li>
				{/each}
			</ul>
			{#if pending.length > 1}
				<div>
					<Button
						onclick={() =>
							ask(acceptVersions([pack.id]), {
								title: m.filters_packs_accept_title({ id: pack.id }),
								applyLabel: m.filters_packs_accept_apply({ id: pack.id })
							})}
						data-testid="pack-accept-all-{pack.id}"
						>{m.filters_packs_accept_all({ id: pack.id })}</Button
					>
				</div>
			{/if}
		</div>
	{/if}

	<details>
		<summary class="cursor-pointer text-small" data-testid="pack-changelog-{pack.id}"
			>{m.filters_packs_changed_heading()}</summary
		>
		<pre
			class="mt-2 whitespace-pre-wrap text-small text-muted-text"
			data-testid="pack-changelog-text-{pack.id}">{pack.changelog}</pre>
	</details>

	<p class="flex flex-wrap items-center gap-2 text-small" data-testid="pack-corpus-{pack.id}">
		<span class="text-muted-text">{m.filters_packs_corpus()}</span>
		{#if corpus === null}
			<Badge>{m.filters_packs_corpus_unknown()}</Badge>
		{:else if corpus.error}
			<Badge variant="warning">{m.filters_packs_corpus_error()}</Badge>
		{:else if failing > 0 || disagree > 0}
			<Badge variant="warning"
				>{m.filters_packs_corpus_failing({ count: failing + disagree })}</Badge
			>
		{:else}
			<Badge variant="success">{m.filters_packs_corpus_ok()}</Badge>
		{/if}
		{#if gaps > 0}<Badge variant="info">{m.filters_packs_corpus_gaps({ count: gaps })}</Badge>{/if}
	</p>

	<ul class="flex flex-col gap-3" data-testid="pack-rules-{pack.id}">
		{#each pack.rules as r (r.id)}
			<RuleRow rule={r} {ask} />
		{/each}
	</ul>
</section>
