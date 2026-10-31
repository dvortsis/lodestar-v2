// The first day of the week for calendars and week-based UI (§10.12). Kept apart from the boot
// path so the start-up bundle doesn't carry it.
const read = (): string => {
	try {
		return JSON.parse(localStorage.getItem('lodestar.region') ?? 'null')?.firstDay ?? 'auto';
	} catch {
		return 'auto';
	}
};
let setting = $state(read());

/** The saved first-day setting (the boot and the settings screen call this). */
export function setFirstDay(firstDay: string): void {
	setting = firstDay;
}

/** `Date#getDay()` numbering: 0 Sunday … 6 Saturday. */
export type WeekDay = 0 | 1 | 6;

/** Day the week starts on: the setting, else the language's own (`Intl.Locale` weekInfo), else Monday. */
export function firstDayOfWeek(locale?: string): WeekDay {
	const first = setting;
	if (first === 'sunday') return 0;
	if (first === 'monday') return 1;
	if (first === 'saturday') return 6;
	try {
		const l = new Intl.Locale(locale ?? navigator.language) as Intl.Locale & {
			getWeekInfo?: () => { firstDay: number };
			weekInfo?: { firstDay: number };
		};
		const d = (l.getWeekInfo?.() ?? l.weekInfo)?.firstDay;
		if (d === 7) return 0;
		if (d === 6) return 6;
	} catch {
		// An old browser or a bad tag: Monday below.
	}
	return 1;
}
