// What the live log shows of the logging settings (§10.10.12): a status chip and, while a data kind
// that identifies torrents or people (or an opt-in) is on, the time left. Read from
// `GET /logs/catalog` (admin session); if it can't be read the page shows neither.
import { api, isAbort } from '$lib/api/client';
import { m } from '$lib/paraglide/messages.js';

export interface LoggingState {
	chip: string;
	/** Unix seconds when the identifying kinds turn off; -1 until turned off; 0 none on. */
	until: number;
	/** The lowest level any event records is debug or lower (a Detailed preset or custom). */
	recordsDebug: boolean;
}

const PRESET: Record<string, () => string> = {
	private: m.logs_preset_private,
	hashes: m.logs_preset_hashes,
	full: m.logs_preset_full,
	custom: m.logs_preset_custom,
	errors: m.logs_preset_errors,
	warnings: m.logs_preset_warnings,
	normal: m.logs_preset_normal,
	detailed: m.logs_preset_detailed,
	everything: m.logs_preset_everything
};
const name = (k: string) => (PRESET[k] ?? m.logs_preset_custom)();

export async function getLoggingState(signal?: AbortSignal): Promise<LoggingState | undefined> {
	try {
		const c = await api.get('/logs/catalog', { signal });
		const ons = c.kinds.filter((k) => (k.identifying || k.opt_in) && k.until !== 0);
		const until = ons.some((k) => k.until === -1) ? -1 : Math.max(0, ...ons.map((k) => k.until));
		return {
			chip: `${name(c.presets.verbosity)} · ${name(c.presets.data)}`,
			until,
			recordsDebug: c.events.some((e) => e.current && (e.level === 'debug' || e.level === 'trace'))
		};
	} catch (e) {
		if (!isAbort(e)) return undefined;
		return undefined;
	}
}

/** "42 min" style text for the banner, or null when nothing identifying is on. */
export function remaining(s: LoggingState, nowMs: number): string | null {
	if (s.until === 0) return null;
	if (s.until === -1) return m.logs_until_off();
	const secs = Math.floor(s.until - nowMs / 1000);
	if (secs <= 0) return null;
	return secs >= 3600
		? m.logs_hours_left({ count: Math.round(secs / 3600) })
		: m.logs_minutes_left({ count: Math.max(1, Math.ceil(secs / 60)) });
}
