// Settings → Overview (§10.10.3): what needs attention, and what differs from the default.
// Pure: the screen fetches, this decides. The settings API sends no default *values* (its schema
// has none), so "changed from default" is `source !== 'default'`, the same test as the field's
// modified marker, and a row says where the value comes from rather than what the default was.
import type { components } from '$lib/api/schema';
import type { FieldKey, FieldSpec, SectionValues, SettingSource } from '../form/types';

type System = components['schemas']['SystemInfo'];
type NetworkCheck = components['schemas']['NetworkCheck'];
type WorkerList = components['schemas']['WorkerList'];
export type StartFailure = components['schemas']['ClusterStartFailure'];

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
	| 'workers_start_failing'
	| 'rebuilding';

export interface Attention {
	kind: AttentionKind;
	severity: 'error' | 'warning' | 'info';
	/**
	 * The Settings screen that fixes it (`/settings/<screen>`), unless `href` says otherwise. None
	 * when no screen does yet: the row has no link.
	 */
	screen?: string;
	/** A page outside Settings (the logs). */
	href?: string;
	count?: number;
	detail?: string;
	/** Why the workers' port couldn't open, and when it's tried next. */
	startFailure?: StartFailure;
}

/** One row per problem the API already reports, most severe first. */
export function needsAttention(
	system: System,
	network?: NetworkCheck,
	screenOf: (section: string) => string | undefined = () => undefined,
	workers?: WorkerList
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
	// Set only while workers are allowed and their port is being tried again; cleared on recovery or
	// when workers are turned off (X-cluster-start-retry). No Workers screen yet (WK-16): no link.
	if (workers?.start_failure)
		out.push({
			kind: 'workers_start_failing',
			severity: 'warning',
			startFailure: workers.start_failure,
			// Where the fix is in config.toml, the row says so; otherwise the logs say why.
			...(USER_FIXES.has(startReason(workers.start_failure)) ? {} : { href: '/logs' })
		});
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

/** What stopped the workers' port, in plain words: the reason a row names. */
export type StartReason =
	| 'port_in_use'
	| 'address'
	| 'permission'
	| 'port'
	| 'database'
	| 'secrets'
	| 'certificates'
	| 'server';

/** The reasons whose row says what to change; the others link to the logs. */
const USER_FIXES: ReadonlySet<StartReason> = new Set(['port_in_use', 'address', 'permission']);

/** The plain-words reason for a start failure: the kind where it says more, else the step. */
export function startReason(f: StartFailure): StartReason {
	switch (f.step) {
		case 'bind':
			if (f.kind === 'addr_in_use') return 'port_in_use';
			if (f.kind === 'addr_not_available') return 'address';
			if (f.kind === 'permission_denied') return 'permission';
			return 'port';
		case 'store':
		case 'pauses':
			return 'database';
		case 'secrets':
			return 'secrets';
		case 'pki':
			return 'certificates';
		case 'server':
			return 'server';
	}
}

/**
 * When a failed start is tried next, for "Trying again in 2 minutes": whole minutes (rounded up),
 * seconds under a minute, and `null` within 45 s ("shortly"). The wait is never over 5 minutes.
 * Kept here, not `formatRelative`, so the shared format chunk every route loads doesn't change.
 */
export function retryIn(
	nextAt: number,
	now: number
): { value: number; unit: 'minute' | 'second' } | null {
	const delta = nextAt - now;
	if (delta < 45) return null;
	if (delta < 60) return { value: Math.round(delta), unit: 'second' };
	return { value: Math.ceil(delta / 60), unit: 'minute' };
}
