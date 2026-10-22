<script lang="ts">
	// Test-only showcase of the overlay primitives (W7cd). Not linked anywhere.
	import { Dialog, Drawer, Menu, Popover, Sheet, Tooltip } from '$lib/ui/overlays';

	let dialogOpen = $state(false);
	let sheetOpen = $state(false);
	let drawerOpen = $state(false);
	let drawerWidth = $state(560);
	let lastMenu = $state('none');
	const rows = ['Ubuntu 24.04 desktop', 'Debian 12 netinst', 'Fedora 40 workstation'];
	let rowEls: HTMLElement[] = [];
	let invokerIndex = $state(0);
</script>

<main class="flex min-h-screen bg-base-200 text-base-content">
	<div class="flex min-w-0 flex-1 flex-col gap-6 p-6">
		<h1 class="text-display font-semibold">Overlay primitives</h1>

		<section class="flex flex-wrap items-center gap-3" aria-labelledby="h-simple">
			<h2 id="h-simple" class="sr-only">Tooltip, popover and menu</h2>
			<Tooltip content="Copies the magnet link to the clipboard" delayDuration={0}>
				{#snippet trigger({ props })}
					<button {...props} type="button" class="btn btn-primary">Copy magnet</button>
				{/snippet}
			</Tooltip>
			<Popover label="Columns">
				{#snippet trigger({ props })}
					<button {...props} type="button" class="btn">Columns</button>
				{/snippet}
				<label class="flex items-center gap-2">
					<input type="checkbox" class="checkbox checkbox-sm" checked />
					Size
				</label>
			</Popover>
			<Menu
				label="Torrent actions"
				onSelect={(id) => (lastMenu = id)}
				items={[
					{ id: 'favorite', label: 'Favorite' },
					{ id: 'hide', label: 'Hide' },
					{ id: 'labels', label: 'Labels' },
					{ id: 'full', label: 'Open full page' },
					{ id: 'fix', label: 'Fix parse', separatorBefore: true }
				]}
			>
				{#snippet trigger({ props })}
					<button {...props} type="button" class="btn">Actions</button>
				{/snippet}
			</Menu>
			<span class="text-small text-muted-text" data-testid="menu-result">{lastMenu}</span>
		</section>

		<section class="flex flex-wrap gap-3" aria-labelledby="h-modal">
			<h2 id="h-modal" class="sr-only">Dialog and sheet</h2>
			<Dialog
				bind:open={dialogOpen}
				title="Remove torrent"
				description="This removes it from your library. It stays discoverable."
				closeLabel="Close"
			>
				{#snippet trigger({ props })}
					<button {...props} type="button" class="btn">Open dialog</button>
				{/snippet}
				<label class="flex flex-col gap-1">
					Reason
					<input class="input" />
				</label>
				{#snippet footer()}
					<button type="button" class="btn" onclick={() => (dialogOpen = false)}>Cancel</button>
					<button type="button" class="btn btn-primary" onclick={() => (dialogOpen = false)}>
						Remove
					</button>
				{/snippet}
			</Dialog>
			<Sheet bind:open={sheetOpen} title="Filters" closeLabel="Close" side="bottom">
				{#snippet trigger({ props })}
					<button {...props} type="button" class="btn">Open sheet</button>
				{/snippet}
				<p>Filters go here.</p>
			</Sheet>
		</section>

		<section aria-labelledby="h-list">
			<h2 id="h-list" class="mb-2 text-title font-semibold">Results</h2>
			<ul class="flex max-w-md flex-col gap-1">
				{#each rows as name, i (name)}
					<li>
						<button
							type="button"
							bind:this={rowEls[i]}
							class="btn w-full justify-start"
							onclick={() => {
								invokerIndex = i;
								drawerOpen = true;
							}}
						>
							{name}
						</button>
					</li>
				{/each}
			</ul>
		</section>
	</div>

	<Drawer
		bind:open={drawerOpen}
		title={rows[invokerIndex] ?? ''}
		closeLabel="Close"
		resizeLabel="Resize detail"
		width={drawerWidth}
		onWidthChange={(w) => (drawerWidth = w)}
		returnFocus={() => rowEls[invokerIndex]}
	>
		<p class="text-muted-text">Width: <span data-testid="drawer-width">{drawerWidth}</span></p>
		<button type="button" class="btn btn-primary mt-4">Copy magnet</button>
	</Drawer>
</main>
