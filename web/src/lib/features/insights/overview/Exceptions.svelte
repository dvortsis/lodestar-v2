<script lang="ts">
	// Level 5, Exceptions (§10.9.2): what needs attention, errors first, each with an icon, one
	// sentence and a fix link. A fix link exists only for a well-formed target.
	import IconError from '~icons/ph/x-circle';
	import IconWarning from '~icons/ph/warning';
	import IconOk from '~icons/ph/check-circle';
	import { formatCount, formatSpan } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import RelativeTime from '$lib/ui/labels/RelativeTime.svelte';
	import { resolve } from '$app/paths';
	import type { Route } from './href';
	import { fixHref, STAGES, type Stage } from './links';
	import Mono from '$lib/ui/labels/Mono.svelte';
	import { sortExceptions, type Exception } from './model';

	let { exceptions }: { exceptions: Exception[] } = $props();

	const locale = $derived(currentLocale());
	const rows = $derived(sortExceptions(exceptions));

	// `fix.kind: "guide"` targets (PK-exc): the user guide pages that cover them.
	const GUIDE = 'https://github.com/dvortsis/lodestar-v2/blob/main/docs/user/';
	const GUIDES: Record<string, string> = {
		data_folder: '02-install-lodestar-on-unraid-and-docker.md',
		windows_ports: '04-install-on-windows-and-linux.md'
	};
	const guideHref = (e: Exception) =>
		e.fix.kind === 'guide' && Object.hasOwn(GUIDES, e.fix.target)
			? GUIDE + GUIDES[e.fix.target]
			: undefined;

	/** The `netsh` command for `ports`, or undefined when no range is large enough. */
	function netsh(e: Exception): string | undefined {
		const p = e.ports;
		if (!p || p.fix_start == null || p.fix_count == null) return undefined;
		return `netsh int ${p.family === 'ipv6' ? 'ipv6' : 'ipv4'} set dynamicport tcp start=${p.fix_start} num=${p.fix_count}`;
	}

	function message(e: Exception): string {
		if (e.kind === 'index_lag')
			return m.insights_overview_exc_index_lag({ lag: formatSpan(e.value, locale) });
		if (e.kind === 'cohort_stalled')
			return m.insights_overview_exc_cohort_stalled({ count: formatCount(e.value, locale) });
		if (e.kind === 'filter_rows_unreadable')
			return m.insights_overview_exc_filter_rows_unreadable({
				count: formatCount(e.value, locale),
				rows: (e.rows ?? []).join(', ')
			});
		if (e.kind === 'filter_stand_ins_rejudged')
			return m.insights_overview_exc_filter_stand_ins_rejudged({
				released: formatCount(e.value, locale),
				kept: formatCount(e.counts?.kept ?? null, locale),
				adult: formatCount(e.counts?.adult ?? null, locale),
				dropped: formatCount(e.counts?.dropped ?? null, locale),
				hidden: formatCount(e.counts?.hidden ?? null, locale)
			});
		if (e.kind === 'data_filesystem') {
			const k = e.filesystem?.kind;
			if (k === 'unraid_user_share') return m.insights_overview_exc_fs_unraid_user_share();
			if (k === 'fuse') return m.insights_overview_exc_fs_fuse();
			if (k === 'network') return m.insights_overview_exc_fs_network();
			return m.insights_overview_exc_fs_unknown();
		}
		if (e.kind === 'ephemeral_ports') {
			const p = e.ports;
			if (!p) return m.insights_overview_exc_ports_bare();
			const left = m.insights_overview_exc_ports_left({
				available: formatCount(p.available, locale),
				needed: formatCount(p.needed, locale)
			});
			return netsh(e)
				? `${left} ${m.insights_overview_exc_ports_fix()}`
				: `${left} ${m.insights_overview_exc_ports_lower()}`;
		}
		return m.insights_overview_exc_unknown({ kind: e.kind });
	}
	function fixText(e: Exception): string {
		if (e.fix.kind === 'settings') return m.insights_overview_exc_fix_settings();
		if (e.fix.kind === 'guide') return m.insights_overview_exc_fix_guide();
		const stage = STAGES.find((s) => s === e.fix.target);
		return stage
			? m.insights_overview_exc_fix_stage({ stage: stageName(stage) })
			: m.insights_overview_exc_fix();
	}
	const stageName = (s: Stage) =>
		({
			discover: m.insights_stage_discover(),
			fetch: m.insights_stage_fetch(),
			understand: m.insights_stage_understand(),
			index: m.insights_stage_index(),
			serve: m.insights_stage_serve()
		})[s];
</script>

{#if rows.length === 0}
	<p class="ins-item-main" data-testid="insights-exceptions-empty">
		<IconOk class="ins-icon" aria-hidden="true" />{m.insights_overview_exc_none()}
	</p>
{:else}
	<ul class="ins-list" data-testid="insights-exceptions">
		{#each rows as e (e.kind + e.since)}
			{@const href = fixHref(e.fix)}
			<li class="ins-item" data-testid="insights-exception-{e.kind}" data-severity={e.severity}>
				<div class="ins-item-main">
					{#if e.severity === 'error'}
						<IconError class="ins-icon" aria-hidden="true" />
					{:else}
						<IconWarning class="ins-icon" aria-hidden="true" />
					{/if}
					<div>
						<p>
							<strong>{e.severity === 'error' ? m.common_error() : m.common_warning()}:</strong>
							{message(e)}
						</p>
						{#if netsh(e)}
							<p data-testid="insights-exception-{e.kind}-netsh">
								<Mono class="break-words">{netsh(e)}</Mono>
							</p>
							<p class="ins-muted">{m.insights_overview_exc_ports_clears()}</p>
						{/if}
						{#if e.filesystem?.fs_type}
							<p class="ins-muted" data-testid="insights-exception-{e.kind}-fs-type">
								{m.insights_overview_exc_fs_type({ type: e.filesystem.fs_type })}
							</p>
						{/if}
						<p class="ins-muted">
							{m.insights_overview_exc_since()}
							<RelativeTime seconds={e.since} timeZone="UTC" />
						</p>
						{#if href}
							<a
								class="ins-link target"
								href={resolve(href as Route)}
								data-testid="insights-exception-{e.kind}-fix">{fixText(e)}</a
							>
						{:else if guideHref(e)}
							<!-- eslint-disable svelte/no-navigation-without-resolve -- external https URL to the user guide, from the fixed GUIDES table -->
							<a
								class="ins-link target"
								href={guideHref(e)}
								target="_blank"
								rel="noopener noreferrer"
								data-testid="insights-exception-{e.kind}-fix"
								>{fixText(e)}<span class="sr-only"> {m.settings_about_new_tab()}</span></a
							>
							<!-- eslint-enable svelte/no-navigation-without-resolve -->
						{/if}
					</div>
				</div>
			</li>
		{/each}
	</ul>
{/if}
