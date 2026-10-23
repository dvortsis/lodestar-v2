// Unit suffixes (`x-unit`), for the number control, validation and display: none of them is in
// a section's first view, so neither are these messages.
import { m } from '$lib/paraglide/messages.js';
import { devWarn } from './schema';

const warned = new Set<string>();

const UNITS: Record<string, () => string> = {
	seconds: m.settings_form_unit_seconds,
	mebibytes: m.settings_form_unit_mebibytes,
	connections: m.settings_form_unit_connections,
	files: m.settings_form_unit_files,
	days: m.settings_form_unit_days
};

/** A unit's suffix ("MiB"); an unknown unit shows as sent, with a dev warning. */
export function unitText(unit: string | undefined): string | undefined {
	if (unit === undefined) return undefined;
	const f = UNITS[unit];
	if (import.meta.env.DEV && !f && !warned.has(unit)) {
		warned.add(unit);
		devWarn(`unknown x-unit "${unit}"`);
	}
	return f ? f() : unit;
}
