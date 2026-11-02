// Settings → Overview (§10.10.3): what needs attention, and what differs from the default.
// Pure: the screen fetches, this decides. The settings API sends no default *values* (its schema
// has none), so "changed from default" is `source !== 'default'`, the same test as the field's
// modified marker, and a row says where the value comes from rather than what the default was.
import type { components } from '$lib/api/schema';
import type { FieldKey, FieldSpec, SectionValues, SettingSource } from '../form/types';

type System = components['schemas']['SystemInfo'];
type NetworkCheck = components['schemas']['NetworkCheck'];

export interface ChangedRow {
	spec: FieldSpec;
	/** Undefined for a secret: its value is never read here, only whether it is set. */
	value?: unknown;
	secret: boolean;
	source: Exclude<SettingSource, 'default'>;
	env: string | null;
}

export interface ChangedGroup {
	/** The screen's id, or the section's name when no screen shows it. */
	id: string;
	/** Whether `/settings/<id>` exists. */
	linked: boolean;
	rows: ChangedRow[];
}

/** The settings that aren't at their default, grouped by the screen that shows them. */
export function changedFromDefault(
	specOf: (key: FieldKey) => FieldSpec | undefined,
	values: readonly SectionValues[],
	screenOf: (section: string) => string | undefined
): ChangedGroup[] {
	const groups = new Map<string, ChangedGroup>();
	for (const section of values) {
		for (const [name, v] of Object.entries(section.fields)) {
			const spec = specOf(`${section.section}.${name}` as FieldKey);
			if (!spec || v.source === 'default') continue;
			const screen = screenOf(section.section);
			const id = screen ?? section.section;
			let group = groups.get(id);
			if (!group) groups.set(id, (group = { id, linked: screen !== undefined, rows: [] }));
			const secret = spec.writeOnly;
			group.rows.push({
				spec,
				secret,
				source: v.source,
				env: v.env,
				...(secret ? {} : { value: v.value })
			});
		}
	}
	return [...groups.values()];
}

export type AttentionKind =
	| 'rebuild_failed'
	| 'address_pending'
	| 'filesystem'
	| 'unreachable'
	| 'restart'
	| 'errors'
	| 'rebuilding';

export interface Attention {
	kind: AttentionKind;
	severity: 'error' | 'warning' | 'info';
	/** The Settings screen that fixes it (`/settings/<screen>`), unless `href` says otherwise. */
	screen: string;
	/** A page outside Settings (the logs). */
	href?: string;
	count?: number;
	detail?: string;
}

/** One row per problem the API already reports, most severe first. */
export function needsAttention(
	system: System,
	network?: NetworkCheck,
	screenOf: (section: string) => string | undefined = () => undefined
): Attention[] {
	const out: Attention[] = [];
	const rebuild = system.data_formats.rebuild;
	if (rebuild?.phase === 'failed')
		out.push({ kind: 'rebuild_failed', severity: 'error', screen: 'about' });
	if (system.address_change?.state === 'awaiting_confirmation')
		out.push({ kind: 'address_pending', severity: 'warning', screen: 'network' });
	if (system.data.filesystem_warning)
		out.push({
			kind: 'filesystem',
			severity: 'warning',
			screen: 'about',
			detail: system.data.filesystem_warning
		});
	if (network?.reachable === false)
		out.push({ kind: 'unreachable', severity: 'warning', screen: 'network' });
	if (system.restart_required.length > 0)
		out.push({
			kind: 'restart',
			severity: 'warning',
			// Where the "apply after a restart" line shows: the first changed setting's screen.
			screen: screenOf(system.restart_required[0]!.split('.')[0]!) ?? 'general',
			count: system.restart_required.length
		});
	const errors = system.diagnostics.recent_errors;
	if (errors !== null && errors > 0)
		out.push({
			kind: 'errors',
			severity: 'warning',
			screen: 'logging',
			href: '/logs',
			count: errors
		});
	if (
		rebuild &&
		(rebuild.phase === 'building' || rebuild.phase === 'finishing' || rebuild.phase === 'swapping')
	)
		out.push({ kind: 'rebuilding', severity: 'info', screen: 'about' });
	const rank = { error: 0, warning: 1, info: 2 } as const;
	return out.sort((a, b) => rank[a.severity] - rank[b.severity]);
}
