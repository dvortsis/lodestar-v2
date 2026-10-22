// Geometry for the sparkline: runs of known samples become path segments, nulls break the line.
export interface SparkGeometry {
	d: string;
	/** Isolated samples (a run of one), drawn as dots because a path of one point is invisible. */
	dots: { x: number; y: number }[];
	/** Number of known samples. */
	points: number;
}

export function sparkPath(
	values: readonly (number | null)[],
	width: number,
	height: number,
	pad = 0
): SparkGeometry {
	const known = values.filter((v): v is number => typeof v === 'number' && Number.isFinite(v));
	if (!known.length) return { d: '', dots: [], points: 0 };
	const lo = Math.min(...known);
	const hi = Math.max(...known);
	const span = hi - lo;
	const step = values.length > 1 ? (width - 2 * pad) / (values.length - 1) : 0;
	const x = (i: number) => (values.length > 1 ? pad + i * step : width / 2);
	// A flat series sits mid-height rather than on the edge.
	const y = (v: number) =>
		span === 0 ? height / 2 : pad + (1 - (v - lo) / span) * (height - 2 * pad);
	const round = (n: number) => Math.round(n * 100) / 100;

	let d = '';
	const dots: SparkGeometry['dots'] = [];
	let run: [number, number][] = [];
	const flush = () => {
		if (run.length === 1) dots.push({ x: round(run[0]![0]), y: round(run[0]![1]) });
		else if (run.length > 1) {
			d += run.map(([px, py], k) => `${k ? 'L' : 'M'}${round(px)} ${round(py)}`).join('');
		}
		run = [];
	};
	values.forEach((v, i) => {
		if (typeof v === 'number' && Number.isFinite(v)) run.push([x(i), y(v)]);
		else flush();
	});
	flush();
	return { d, dots, points: known.length };
}
