// Settings → About's pure parts (§10.10.13): the stale-tab check, the plain-text version and
// diagnostics blocks, and the links built from the build's repository. Kept free of the DOM so they
// are unit-tested alone.
import type { components } from '$lib/api/schema';

export type System = components['schemas']['SystemInfo'];

/**
 * The tab's UI came from a different build than the server runs. Only when both ids are known: a
 * stub page or a server that doesn't say (null) is never "older".
 */
export function isStale(ui: string | null | undefined, server: string | null | undefined): boolean {
	return !!ui && !!server && ui !== server;
}

/** `https://host/owner/repo` from the build's `repository`, or null when it isn't a plain https URL. */
export function repoBase(repository: string | null | undefined): string | null {
	const raw = (repository ?? '')
		.trim()
		.replace(/\/+$/, '')
		.replace(/\.git$/, '');
	try {
		const url = new URL(raw);
		return url.protocol === 'https:' && !url.username && !url.search && !url.hash ? raw : null;
	} catch {
		return null;
	}
}

export interface Links {
	repo: string;
	commit: string | null;
	source: string;
	docs: string;
	releases: string;
	problem: string;
	feature: string;
	discussions: string;
	security: string;
}

/** The help and community links; null when the build has no usable repository. */
export function links(
	info: Pick<System, 'repository' | 'build' | 'channel' | 'version'>
): Links | null {
	const repo = repoBase(info.repository);
	if (!repo) return null;
	const commit = info.build.commit;
	// The exact tag for a release; the commit for a nightly, a dev build or a fork's untagged build.
	const release = info.channel === 'stable' || info.channel === 'beta';
	return {
		repo,
		commit: commit ? `${repo}/commit/${commit}` : null,
		source: release ? `${repo}/tree/v${info.version}` : commit ? `${repo}/tree/${commit}` : repo,
		docs: `${repo}/tree/main/docs`,
		releases: `${repo}/releases`,
		problem: `${repo}/issues/new`,
		feature: `${repo}/issues/new?labels=enhancement`,
		discussions: `${repo}/discussions`,
		security: `${repo}/security/advisories/new`
	};
}

export const shortCommit = (commit: string | null | undefined): string | null =>
	commit ? commit.slice(0, 9) : null;

const day = (seconds: number | null | undefined): string | null =>
	typeof seconds === 'number' ? new Date(seconds * 1000).toISOString().slice(0, 10) : null;

const line = (label: string, value: string | number | null | undefined): string[] =>
	value === null || value === undefined || value === '' ? [] : [`${label}: ${value}`];

/** "Copy version info": the version and build details as plain text. */
export function versionText(info: System, uiBuild: string): string {
	const b = info.build;
	return [
		`Lodestar ${info.version}`,
		...line('Channel', info.channel),
		...line('Commit', b.commit),
		...line('Build date', day(b.date)),
		...line('Target', b.target),
		...line('Rust', b.rustc),
		...line('UI build', b.ui_build_id ?? uiBuild)
	].join('\n');
}

/**
 * The diagnostics block (§12.1): versions, platform, deployment, data formats and which settings
 * differ from their defaults, by name and source. Never a setting's value, the data directory, a
 * path, the time zone or a torrent: what a bug report needs and nothing that identifies a person.
 */
export function diagnosticsText(info: System, uiBuild: string): string {
	const h = info.host;
	const d = info.deployment;
	const f = info.data_formats;
	const g = info.diagnostics;
	const mem = h.memory_bytes === null ? null : `${Math.round(h.memory_bytes / 2 ** 20)} MiB`;
	const changed = g.changed.map((c) => `${c.name} (${c.source})`);
	return [
		versionText(info, uiBuild),
		'',
		...line('OS', `${h.os} ${h.arch}`),
		...line('CPUs', h.cpus),
		...line('Memory', mem),
		...line('Deployment', d.kind),
		...line('Image tag', d.image_tag),
		...line('Image digest', d.image_digest),
		...line('Store schema', f.store_schema),
		...line('Search index', f.index),
		...line('Uptime', `${Math.round(info.uptime_s)} s`),
		...line('Logging preset', g.logging),
		...line('Recent errors', g.recent_errors),
		`Changed settings: ${changed.length ? changed.join(', ') : 'none'}`
	].join('\n');
}

/** Longest URL the "Report a problem" link carries; past it the text is copied instead. */
export const URL_LIMIT = 6000;

/**
 * The prefilled new-issue URL for the text shown in the dialog, or null when it would be too long
 * (or there is no repository): the caller copies the text and opens the plain issue page.
 */
export function problemUrl(base: string | null, body: string): string | null {
	if (!base) return null;
	const url = `${base}/issues/new?body=${encodeURIComponent(body)}`;
	return url.length <= URL_LIMIT ? url : null;
}
