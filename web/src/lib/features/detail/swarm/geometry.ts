// Geometry for the Swarm line (§10.5.4): time on x, peers on y from zero. A missing sample (null, or
// a bucket the server sent nothing for) breaks the line: gaps, never zeros.
export interface Sample {
	at: number;
	seeders: number | null;
	leechers: number | null;
}
export interface Line {
	key: 'seeders' | 'leechers';
	/** Path data: one sub-path per unbroken run. */
	d: string;
	/** Runs of one sample, which a path can't show. */
	dots: { x: number; y: number }[];
}
export interface Chart {
	lines: Line[];
	max: number;
	from: number;
	to: number;
	points: { at: number; x: number; seeders: number | null; leechers: number | null }[];
}

const round = (n: number) => Math.round(n * 100) / 100;

export function buildChart(
	samples: readonly Sample[],
	bucketSeconds: number,
	width: number,
	height: number,
	pad = 4
): Chart | null {
	const list = [...samples].sort((a, b) => a.at - b.at);
	const known = list.flatMap((s) => [s.seeders, s.leechers]).filter((v): v is number => v !== null);
	if (known.length === 0) return null;
	const max = Math.max(...known, 1);
	const from = list[0]!.at;
	const to = list[list.length - 1]!.at;
	const x = (at: number) =>
		to === from ? width / 2 : pad + ((at - from) / (to - from)) * (width - 2 * pad);
	const y = (v: number) => pad + (1 - v / max) * (height - 2 * pad);
	// More than one and a half buckets between two samples is a missing one.
	const gap = bucketSeconds * 1.5;
	const lines = (['seeders', 'leechers'] as const).map((key): Line => {
		let d = '';
		const dots: Line['dots'] = [];
		let run: [number, number][] = [];
		const flush = () => {
			if (run.length === 1) dots.push({ x: round(run[0]![0]), y: round(run[0]![1]) });
			else if (run.length > 1)
				d += run.map(([px, py], i) => `${i ? 'L' : 'M'}${round(px)} ${round(py)}`).join('');
			run = [];
		};
		let prev: number | null = null;
		for (const s of list) {
			const v = s[key];
			if (v === null || (prev !== null && s.at - prev > gap)) flush();
			if (v !== null) run.push([x(s.at), y(v)]);
			prev = v === null ? null : s.at;
		}
		flush();
		return { key, d, dots };
	});
	return {
		lines,
		max,
		from,
		to,
		points: list.map((s) => ({
			at: s.at,
			x: round(x(s.at)),
			seeders: s.seeders,
			leechers: s.leechers
		}))
	};
}
