// Which time zone an Insights value is shown in (docs/metrics.md §1–2, X-labels U28): an instant
// or an hour bucket in the display zone (the Language & region choice, or the browser's for
// Automatic); a day or week bucket is a UTC day on the server, so it's shown, and named, in UTC.

/** The zone for a value of this bucket (`undefined` for an instant): `display`, or UTC for days. */
export function bucketTimeZone(bucket: string | undefined, display: string): string {
	return bucket === 'day' || bucket === 'week' ? 'UTC' : display;
}
