<script lang="ts">
	// The open-source components list (§10.10.13): the build's content-hashed licenses.json, fetched
	// when the section is opened.
	import { licensesUrl } from 'virtual:licenses';
	import { m } from '$lib/paraglide/messages.js';
	import Input from '$lib/ui/fields/Input.svelte';

	interface Doc {
		npm: { name: string; version: string; license: string }[];
		rust: { crates: unknown[] };
	}
	interface Row {
		name: string;
		version: string;
		license: string;
	}

	let rows = $state.raw<Row[]>();
	let failed = $state(false);
	let query = $state('');

	$effect(() => {
		let live = true;
		fetch(licensesUrl)
			.then((r) =>
				r.ok ? (r.json() as Promise<Doc>) : Promise.reject(new Error(String(r.status)))
			)
			.then(
				(d) => {
					if (!live) return;
					const crates = d.rust.crates.map((c) => {
						const x = c as { name?: string; version?: string; license?: string };
						return { name: x.name ?? '', version: x.version ?? '', license: x.license ?? '' };
					});
					rows = [...d.npm, ...crates].sort((a, b) => a.name.localeCompare(b.name));
				},
				() => live && (failed = true)
			);
		return () => (live = false);
	});

	const shown = $derived(
		(rows ?? []).filter((r) =>
			`${r.name} ${r.license}`.toLowerCase().includes(query.trim().toLowerCase())
		)
	);
</script>

{#if failed}
	<p role="alert" class="text-error">{m.settings_about_licence_error()}</p>
{:else if !rows}
	<p aria-busy="true" role="status">{m.settings_about_licence_loading()}</p>
{:else}
	<Input type="search" aria-label={m.settings_about_licence_search()} bind:value={query} />
	<p role="status" class="text-small text-muted-text">
		{m.settings_about_licence_count({ count: shown.length })}
	</p>
	<ul>
		{#each shown.slice(0, 100) as r (r.name + r.version)}
			<li class="font-mono text-small">{r.name} {r.version} · {r.license}</li>
		{/each}
	</ul>
{/if}
