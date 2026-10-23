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
	import { sortExceptions, type Exception } from './model';

	let { exceptions }: { exceptions: Exception[] } = $props();

	const locale = $derived(currentLocale());
	const rows = $derived(sortExceptions(exceptions));

	function message(e: Exception): string {
		if (e.kind === 'index_lag')
			return m.insights_overview_exc_index_lag({ lag: formatSpan(e.value, locale) });
		if (e.kind === 'cohort_stalled')
			return m.insights_overview_exc_cohort_stalled({ count: formatCount(e.value, locale) });
		return m.insights_overview_exc_unknown({ kind: e.kind });
	}
	function fixText(e: Exception): string {
		if (e.fix.kind === 'settings') return m.insights_overview_exc_fix_settings();
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
						<p class="ins-muted">
							{m.insights_overview_exc_since()}
							<RelativeTime seconds={e.since} timeZone="UTC" />
						</p>
						{#if href}
							<a
								class="ins-link"
								href={resolve(href as Route)}
								data-testid="insights-exception-{e.kind}-fix">{fixText(e)}</a
							>
						{/if}
					</div>
				</div>
			</li>
		{/each}
	</ul>
{/if}
