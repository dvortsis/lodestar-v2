<script lang="ts">
	// Test-only showcase of the filters change flows (M-filters-dialogs), at /dev/filters-flows. Not
	// linked anywhere. Each button runs one change through the real flow and `ChangeDialog`; the
	// e2e specs answer the API with the OpenAPI mock. The dialogs load lazily, as screens load them.
	import Button from '$lib/ui/controls/Button.svelte';
	import {
		addAllow,
		addException,
		addRule,
		confirmAbuse,
		coreSwitch,
		createAdultConfirm,
		createChangeFlow,
		falsePositive,
		floorRemoval,
		packSwitch,
		release,
		removeAllow,
		removeCs1,
		removeException,
		removeRule,
		reportAbuse,
		setShowAdult,
		undoChange,
		updateRule,
		adultErrorText,
		type Change
	} from '$lib/features/filters/flows';

	const HASH = '0123456789abcdef0123456789abcdef01234567';

	interface Case {
		id: string;
		label: string;
		change: Change;
		title: string;
		applyLabel?: string;
		shadowDays?: number;
	}

	const cases: Case[] = [
		{
			id: 'add-rule',
			label: 'Add a hide rule',
			change: addRule({ query: `hash:${HASH}`, action: 'hide' }),
			title: 'Add a rule',
			shadowDays: 7
		},
		{
			id: 'core-off',
			label: 'Turn off CS-4a',
			change: coreSwitch('CS-4a', false),
			title: 'Turn off CS-4a',
			applyLabel: 'Turn off CS-4a'
		},
		{
			id: 'floor-off',
			label: 'Turn off CS-1a (locked floor)',
			change: coreSwitch('CS-1a', false),
			title: 'Turn off CS-1a'
		},
		{
			id: 'core-on',
			label: 'Turn on CS-4a',
			change: coreSwitch('CS-4a', true),
			title: 'Turn on CS-4a'
		},
		{
			id: 'pack-off',
			label: 'Turn off a pack',
			change: packSwitch('malware', false),
			title: 'Turn off Malware'
		},
		{
			id: 'update-rule',
			label: 'Edit a rule',
			change: updateRule(3, { query: 'name:"sample"' }),
			title: 'Edit rule U-3',
			shadowDays: 7
		},
		{ id: 'remove-rule', label: 'Remove a rule', change: removeRule(3), title: 'Remove rule U-3' },
		{
			id: 'add-exception',
			label: 'Add an exception',
			change: addException({ hashes: [HASH] }, 'CS-4a'),
			title: 'Add an exception'
		},
		{
			id: 'remove-exception',
			label: 'Remove an exception',
			change: removeException(2),
			title: 'Remove an exception'
		},
		{
			id: 'add-allow',
			label: 'Always allow',
			change: addAllow('type:movie'),
			title: 'Always allow'
		},
		{
			id: 'remove-allow',
			label: 'Remove Always allow',
			change: removeAllow(2),
			title: 'Remove from Always allow'
		},
		{
			id: 'release',
			label: 'Release from quarantine',
			change: release(HASH),
			title: 'Release this torrent'
		},
		{
			id: 'confirm',
			label: 'Confirm as abuse',
			change: confirmAbuse(HASH),
			title: 'Confirm as abuse material'
		},
		{
			id: 'false-positive',
			label: 'False positive',
			change: falsePositive(HASH),
			title: 'Mark as a false positive'
		},
		{
			id: 'report',
			label: 'Report abuse',
			change: reportAbuse([HASH]),
			title: 'Report as child-abuse material'
		},
		{
			id: 'remove-cs1',
			label: 'Remove from CS-1',
			change: removeCs1([HASH]),
			title: 'Remove from CS-1'
		},
		{
			id: 'floor-remove',
			label: 'Remove floor entries',
			change: floorRemoval('file:/config/blocklist.txt', true),
			title: 'Remove floor entries'
		},
		{
			id: 'floor-keep',
			label: 'Keep floor entries',
			change: floorRemoval('file:/config/blocklist.txt', false),
			title: 'Keep floor entries'
		},
		{
			id: 'undo',
			label: 'Undo the last change',
			change: undoChange(),
			title: 'Undo the last change'
		}
	];

	const flow = createChangeFlow();
	let current = $state<Case | null>(null);
	let last = $state('none');

	const dialogs = () =>
		Promise.all([
			import('$lib/features/filters/flows/ChangeDialog.svelte'),
			import('$lib/features/filters/flows/AdultConfirmDialog.svelte'),
			import('$lib/features/filters/flows/OverrideDialog.svelte'),
			import('$lib/features/filters/flows/BlockThisDialog.svelte'),
			import('$lib/features/filters/flows/RevealNameDialog.svelte')
		]);
	let loaded = $state<Awaited<ReturnType<typeof dialogs>> | null>(null);
	async function ensure() {
		loaded ??= await dialogs();
	}

	async function run(c: Case) {
		await ensure();
		current = c;
		flow.start(c.change);
	}

	// Part 2: each dialog owns its flow; the showcase only opens it.
	let overrideOpen = $state(false);
	let blockOpen = $state(false);
	let revealOpen = $state(false);
	const torrent = {
		hash: HASH,
		hash_v2: 'fedcba9876543210'.repeat(4),
		name: '[Grp] Some.Show.S01E02.1080p.WEB'
	};
	async function openDialog(which: 'override' | 'block' | 'reveal') {
		await ensure();
		if (which === 'override') overrideOpen = true;
		else if (which === 'block') blockOpen = true;
		else revealOpen = true;
	}

	const adult = createAdultConfirm();
	let adultStatus = $state('');
	async function showAdult(on: boolean) {
		await ensure();
		const r = await setShowAdult(on, adult);
		adultStatus = r.ok
			? on
				? 'Adult content shown'
				: 'Adult content hidden'
			: r.error
				? adultErrorText(r.error)
				: 'Cancelled';
	}
</script>

<div
	class="mx-auto flex max-w-(--manuscript-width) flex-col gap-6 bg-base-200 p-6 text-base-content"
>
	<h1 class="text-display font-semibold">Filter change flows</h1>

	<section class="flex flex-col gap-3" aria-labelledby="h-changes">
		<h2 id="h-changes" class="text-title font-semibold">Changes</h2>
		<div class="flex flex-wrap gap-2">
			{#each cases as c (c.id)}
				<Button dense onclick={() => run(c)} data-testid="case-{c.id}">{c.label}</Button>
			{/each}
		</div>
		<p role="status" data-testid="last-applied">Last applied: {last}</p>
	</section>

	<section class="flex flex-col gap-3" aria-labelledby="h-dialogs">
		<h2 id="h-dialogs" class="text-title font-semibold">Dialogs</h2>
		<div class="flex flex-wrap gap-2">
			<Button dense onclick={() => openDialog('override')} data-testid="open-override"
				>Add to the override list</Button
			>
			<Button dense onclick={() => openDialog('block')} data-testid="open-block">Block this…</Button
			>
			<Button dense onclick={() => openDialog('reveal')} data-testid="open-reveal"
				>Reveal the sealed name</Button
			>
		</div>
	</section>

	<section class="flex flex-col gap-3" aria-labelledby="h-adult">
		<h2 id="h-adult" class="text-title font-semibold">Show adult content</h2>
		<div class="flex flex-wrap gap-2">
			<Button dense onclick={() => showAdult(true)} data-testid="adult-on">Turn on</Button>
			<Button dense onclick={() => showAdult(false)} data-testid="adult-off">Turn off</Button>
		</div>
		<p role="status" data-testid="adult-status">{adultStatus}</p>
	</section>
</div>

{#if loaded}
	{@const [
		{ default: ChangeDialog },
		{ default: AdultConfirmDialog },
		{ default: OverrideDialog },
		{ default: BlockThisDialog },
		{ default: RevealNameDialog }
	] = loaded}
	<ChangeDialog
		{flow}
		title={current?.title ?? ''}
		applyLabel={current?.applyLabel}
		shadowDays={current?.shadowDays}
		onapplied={(r) => (last = `${current?.id} v${r.version}`)}
	/>
	<AdultConfirmDialog confirm={adult} />
	<OverrideDialog
		bind:open={overrideOpen}
		shadowDays={7}
		onapplied={(r) => (last = `override v${r.version}`)}
	/>
	<BlockThisDialog
		{torrent}
		bind:open={blockOpen}
		onapplied={(r) => (last = `block v${r.version}`)}
	/>
	<RevealNameDialog hash={HASH} bind:open={revealOpen} />
{/if}
