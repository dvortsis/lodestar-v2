<script lang="ts">
	import {
		CategoryBar,
		DataTableAlternative,
		HBarList,
		Histogram,
		Kpi,
		LineChart,
		Sparkline,
		type BrushRange
	} from '$lib/charts';
	import { formatBytes, formatDate, formatShare, sizeBinEdges } from '$lib/format';

	// Test-only showcase of the chart primitives with fixed data. Not linked anywhere.
	const segments = [
		{ group: 'video' as const, value: 820, label: 'Video' },
		{ group: 'audio' as const, value: 90, label: 'Audio' },
		{ group: 'images' as const, value: 20, label: 'Images' },
		{ group: 'documents' as const, value: 30, label: 'Documents' },
		{ group: 'archives' as const, value: 25, label: 'Archives' },
		{ group: 'software' as const, value: 10, label: 'Software' },
		{ group: 'other' as const, value: 5, label: 'Other' }
	];
	const small = [
		{ group: 'video' as const, value: 300, label: 'Video' },
		{ group: 'software' as const, value: 100, label: 'Software' }
	];
	const hbar = [
		{ key: 'a', label: 'Ubuntu releases', value: 4200, share: 0.42 },
		{ key: 'b', label: 'Debian ISOs', value: 2800, share: 0.28 },
		{ key: 'c', label: 'Arch snapshots', value: 900, share: 0.09 },
		{ key: 'd', label: 'Unscraped', value: null }
	];
	const bins = sizeBinEdges(20, 28);
	const counts = [0, 12, 40, 95, 160, 70, null, 4];
	let selection = $state<BrushRange | null>(null);

	const DAY = 86400;
	const start = 1760918400;
	const times = Array.from({ length: 14 }, (_, i) => start + i * DAY);
	const seeders = [120, 130, 128, null, null, 150, 160, 158, 170, 180, 175, 190, 200, 210];
	const rows = times.map((t, i) => ({
		day: formatDate(t, 'en', { timeZone: 'UTC' }),
		n: seeders[i] == null ? '—' : String(seeders[i])
	}));
	let table = $state(false);
</script>

<div
	class="mx-auto flex max-w-(--manuscript-width) flex-col gap-8 bg-base-200 p-6 text-base-content"
>
	<h1 class="text-display font-semibold">Chart primitives</h1>

	<section class="grid grid-cols-2 gap-6" aria-label="KPIs">
		<Kpi
			label="Indexed torrents"
			value={1284000}
			delta={{ value: 3200, text: 'vs previous 7 days' }}
			sparkline={{ values: [3, 4, null, 6, 7, 9], label: 'Indexed torrents, last 6 days' }}
			testid="kpi-indexed"
		/>
		<Kpi
			label="DHT size"
			value={{ value: 2400000, approx: true }}
			delta={{ value: -0.032, kind: 'percent', text: 'vs previous 7 days' }}
			testid="kpi-dht"
		/>
		<Kpi label="Never scraped" value={null} testid="kpi-null" />
		<Kpi
			label="Junk"
			value={0}
			delta={{ value: 0, text: 'vs previous 7 days' }}
			testid="kpi-zero"
		/>
	</section>

	<section class="flex flex-col gap-3" aria-label="Composition">
		<CategoryBar
			{segments}
			summary="82% video, 9% audio, 3% documents, 2.5% archives, 2% images, 1% software, 0.5% other"
			dominant="{formatShare(82)} video"
			testid="compbar"
		/>
		<div class="flex flex-col gap-2">
			<CategoryBar
				segments={small}
				summary="75% video, 25% software"
				dominant="75% video"
				max={1000}
				testid="compbar-small"
			/>
			<CategoryBar
				{segments}
				summary="82% video, 9% audio"
				dominant="82% video"
				max={1000}
				testid="compbar-large"
			/>
		</div>
	</section>

	<section aria-label="Top groups">
		<HBarList items={hbar} label="Top version groups" testid="hbars" />
	</section>

	<section class="flex flex-col gap-2" aria-label="Sizes">
		<Histogram
			{counts}
			edges={bins.labels}
			label="Torrents by size"
			handleLabels={{ from: 'Smallest size', to: 'Largest size' }}
			bind:selection
			testid="hist"
		/>
		<p
			class="text-small text-muted-text"
			data-testid="hist-range"
			data-range={selection ? `${selection.from}-${selection.to}` : 'none'}
		>
			{selection ? `${bins.labels[selection.from]} – ${bins.labels[selection.to]}` : 'No range'}
		</p>
		<p class="text-small tabular-nums">{formatBytes(1048576)}</p>
	</section>

	<section class="flex flex-col gap-2" aria-label="Trend">
		<LineChart
			{times}
			series={[{ label: 'Seeders', values: seeders }]}
			label="Seeders rose from 120 to 210 over 14 days, with a two-day gap"
			unit="seeders"
			timeZone="UTC"
			testid="line"
		/>
		<button
			type="button"
			class="btn btn-sm self-start"
			aria-pressed={table}
			onclick={() => (table = !table)}
			data-testid="line-toggle">View as table</button
		>
		<DataTableAlternative
			caption="Seeders per day"
			columns={[
				{ key: 'day', label: 'Day' },
				{ key: 'n', label: 'Seeders', numeric: true }
			]}
			{rows}
			visible={table}
			testid="line-table"
		/>
		<div class="flex items-center gap-3">
			<Sparkline values={[1, 3, null, null, 5, 2]} label="Gap sparkline" testid="spark" />
		</div>
	</section>
</div>
