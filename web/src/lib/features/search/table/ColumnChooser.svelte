<script lang="ts">
	// The column chooser (§10.4): show or hide each column and move it earlier or later. Loaded on
	// its first open. Esc closes it and puts focus back on the button.
	import ArrowDown from '~icons/ph/arrow-down-bold';
	import ArrowUp from '~icons/ph/arrow-up-bold';
	import { m } from '$lib/paraglide/messages.js';
	import Checkbox from '$lib/ui/fields/Checkbox.svelte';
	import IconButton from '$lib/ui/controls/IconButton.svelte';
	import {
		REQUIRED,
		defaultPrefs,
		moveColumn,
		toggleColumn,
		type ColumnId,
		type ColumnPrefs
	} from './columns';

	interface Props {
		prefs: ColumnPrefs;
		onchange: (next: ColumnPrefs) => void;
		onclose: () => void;
	}
	let { prefs, onchange, onclose }: Props = $props();

	const LABEL: Record<ColumnId, () => string> = {
		name: m.search_table_col_name,
		type: m.search_table_col_type,
		size: m.search_table_col_size,
		files: m.search_table_col_files,
		seeders: m.seeders,
		leechers: m.leechers,
		trackers: m.trackers_reporting,
		discovered: m.discovered
	};
</script>

<!-- Moving a column moves its focused button in the DOM, which drops focus: Esc is heard on the window. -->
<svelte:window onkeydown={(e) => e.key === 'Escape' && onclose()} />
<div
	class="rt-chooser"
	role="group"
	aria-label={m.search_table_columns()}
	data-testid="table-chooser"
>
	<ul>
		{#each prefs.order as id, i (id)}
			<li>
				<Checkbox
					label={LABEL[id]()}
					checked={!prefs.hidden.includes(id)}
					disabled={id === REQUIRED}
					onchange={() => onchange(toggleColumn(prefs, id))}
					data-testid="table-col-{id}"
				/>
				<span class="rt-move">
					<IconButton
						dense
						label={m.search_table_move_earlier({ column: LABEL[id]() })}
						disabled={i === 0}
						onclick={() => onchange(moveColumn(prefs, id, -1))}><ArrowUp /></IconButton
					>
					<IconButton
						dense
						label={m.search_table_move_later({ column: LABEL[id]() })}
						disabled={i === prefs.order.length - 1}
						onclick={() => onchange(moveColumn(prefs, id, 1))}><ArrowDown /></IconButton
					>
				</span>
			</li>
		{/each}
	</ul>
	<button type="button" class="rt-reset" onclick={() => onchange(defaultPrefs())}
		>{m.search_table_reset()}</button
	>
</div>
