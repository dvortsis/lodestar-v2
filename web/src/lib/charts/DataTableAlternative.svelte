<script lang="ts">
	interface Column {
		key: string;
		label: string;
		/** Numeric columns align right. */
		numeric?: boolean;
	}
	interface Props {
		caption: string;
		columns: Column[];
		/** Cells arrive already formatted through `$lib/format` ("—" for unknowns). */
		rows: Record<string, string>[];
		/** Visible (the "View as table" state) or only exposed to assistive tech. */
		visible?: boolean;
		testid?: string;
	}
	let { caption, columns, rows, visible = false, testid = 'data-table' }: Props = $props();
</script>

<div class={visible ? 'overflow-x-auto' : 'sr-only'} data-testid={testid}>
	<table class="w-full text-left text-small">
		<caption class="py-2 text-left text-muted-text">{caption}</caption>
		<thead>
			<tr>
				{#each columns as c (c.key)}
					<th scope="col" class="px-2 py-1 font-medium {c.numeric ? 'text-right' : ''}"
						>{c.label}</th
					>
				{/each}
			</tr>
		</thead>
		<tbody>
			{#each rows as row, i (i)}
				<tr class="border-t border-base-300">
					{#each columns as c (c.key)}
						<td class="px-2 py-1 {c.numeric ? 'text-right tabular-nums' : ''}"
							>{row[c.key] ?? '—'}</td
						>
					{/each}
				</tr>
			{/each}
		</tbody>
	</table>
</div>
