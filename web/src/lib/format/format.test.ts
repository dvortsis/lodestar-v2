import { describe, expect, it } from 'vitest';
import {
	DASH,
	formatBytes,
	formatBytesParts,
	formatCompact,
	formatCount,
	formatDate,
	formatDateFull,
	formatDateTime,
	formatDelta,
	formatLatency,
	formatLatencyParts,
	formatPercent,
	formatRate,
	formatRelative,
	formatScore,
	formatShare,
	formatSpan,
	formatSpanParts,
	sizeBinEdges,
	sizeBinLabel
} from './index';

const NB = ' ';
const MINUS = '−';
const UTC = { timeZone: 'UTC' };
// 2026-10-22 14:05 UTC, a Thursday (metrics.md §3).
const T = Date.UTC(2026, 9, 22, 14, 5) / 1000;

type Row = [input: number, en: string, zh?: string];
function table(name: string, fn: (v: number, l: string) => string, rows: Row[]) {
	describe(name, () => {
		for (const [input, en, zh] of rows) {
			it(`${input} → ${en}`, () => {
				expect(fn(input, 'en')).toBe(en);
				expect(fn(input, 'zh')).toBe(zh ?? en);
			});
		}
	});
}

// Every unknown or non-finite input renders "—" and never throws, in every formatter.
const BAD = [null, undefined, NaN, Infinity, -Infinity];
const numeric: [string, (v: never, l?: string) => string][] = [
	['formatCount', formatCount],
	['formatCompact', formatCompact],
	['formatBytes', formatBytes],
	['formatPercent', formatPercent],
	['formatShare', formatShare],
	['formatRate', formatRate],
	['formatDelta', formatDelta],
	['formatLatency', formatLatency],
	['formatSpan', formatSpan],
	['formatScore', formatScore]
];
describe('value states', () => {
	for (const [name, fn] of numeric) {
		it(`${name} renders — for null, undefined, NaN, ±Infinity`, () => {
			for (const bad of BAD) {
				for (const locale of ['en', 'zh']) {
					expect(fn(bad as never, locale)).toBe(DASH);
					expect(fn({ value: bad, approx: true } as never, locale)).toBe(DASH);
				}
			}
		});
	}
	it('time formatters render — for unknown or invalid timestamps', () => {
		for (const bad of [...BAD, 1e20, -1e20]) {
			expect(formatDateTime(bad as never, 'en', UTC)).toBe(DASH);
			expect(formatDateFull(bad as never, 'en', UTC)).toBe(DASH);
			expect(formatDate(bad as never, 'en', UTC)).toBe(DASH);
		}
		for (const bad of BAD) expect(formatRelative(bad as never, 'en', { now: T })).toBe(DASH);
	});
	it('a real 0 is 0, never —', () => {
		expect(formatCount(0)).toBe('0');
		expect(formatCompact(0)).toBe('0');
		expect(formatBytes(0)).toBe(`0${NB}B`);
		expect(formatPercent(0)).toBe('0.0%');
		expect(formatShare(0)).toBe('0%');
		expect(formatRate(0)).toBe('0/h');
		expect(formatDelta(0)).toBe('0');
		expect(formatLatency(0)).toBe('0 ms');
		expect(formatSpan(0)).toBe('0 sec');
		expect(formatScore(0)).toBe('0');
	});
	it('estimated values get ≈ and a no-break space', () => {
		expect(formatCompact({ value: 2.4e6, approx: true })).toBe(`≈${NB}2.4M`);
		expect(formatCount({ value: 1225, approx: true })).toBe(`≈${NB}1,225`);
		expect(formatCount({ value: 0, approx: true })).toBe(`≈${NB}0`);
		expect(formatCount({ value: 5, approx: false })).toBe('5');
	});
});

table('formatCount', formatCount, [
	[0, '0'],
	[1284, '1,284'],
	[84210, '84,210'],
	[1e12, '1,000,000,000,000'],
	[1284.6, '1,285'],
	[-5, '-5']
]);

table('formatCompact', formatCompact, [
	[950, '950'],
	[1200, '1.2K', '1200'],
	[84200, '84.2K', '8.4万'],
	[2.4e6, '2.4M', '240万'],
	[12.4e6, '12.4M', '1240万'],
	[999, '999'],
	[1000, '1K', '1000'],
	[1e12, '1T', '1万亿'],
	[0, '0']
]);

describe('formatBytes', () => {
	const rows: [number, string][] = [
		[512, `512${NB}B`],
		[4.37 * 1024 ** 3, `4.37${NB}GiB`],
		[38.2 * 1024 ** 2, `38.2${NB}MiB`],
		[912 * 1024, `912${NB}KiB`],
		[1024 ** 2, `1.00${NB}MiB`],
		// Unit switches.
		[0, `0${NB}B`],
		[1023, `1,023${NB}B`],
		[1024, `1.00${NB}KiB`],
		[1024 ** 2 - 1, `1.00${NB}MiB`], // 1023.999 KiB rounds to 1024: steps up, never "1024 KiB"
		[1023.5 * 1024, `1.00${NB}MiB`],
		[1023.4 * 1024, `1,023${NB}KiB`],
		[1024 ** 3, `1.00${NB}GiB`],
		[1024 ** 4, `1.00${NB}TiB`],
		[1024 ** 5, `1.00${NB}PiB`],
		// Digits by magnitude.
		[9.5 * 1024, `9.50${NB}KiB`],
		[10 * 1024, `10.0${NB}KiB`],
		[99.5 * 1024 ** 2, `99.5${NB}MiB`],
		// Digits follow the rounded value, so columns stay aligned.
		[9.996 * 1024 ** 3, `10.0${NB}GiB`],
		[9.994 * 1024 ** 3, `9.99${NB}GiB`],
		[99.96 * 1024 ** 2, `100${NB}MiB`],
		[99.94 * 1024 ** 2, `99.9${NB}MiB`],
		[1023.96 * 1024, `1.00${NB}MiB`],
		[100 * 1024 ** 2, `100${NB}MiB`],
		// Beyond PiB stays PiB.
		[1500 * 1024 ** 5, `1,500${NB}PiB`],
		[1e30, `888,178,419,700,125${NB}PiB`]
	];
	for (const [input, expected] of rows) {
		it(`${input} → ${expected}`, () => {
			expect(formatBytes(input, 'en')).toBe(expected);
			expect(formatBytes(input, 'zh')).toBe(expected);
		});
	}
	it('negative sizes are —', () => {
		expect(formatBytes(-1)).toBe(DASH);
	});
	it('parts split number and unit', () => {
		expect(formatBytesParts(4.37 * 1024 ** 3)).toEqual({ value: '4.37', unit: 'GiB' });
		expect(formatBytesParts(null)).toEqual({ value: DASH, unit: '' });
	});
	it('never uses SI symbols', () => {
		expect(formatBytes(1e6)).not.toMatch(/[kMG]B/);
	});
});

describe('formatPercent', () => {
	const rows: [number, 0 | 1, string][] = [
		[0.374, 1, '37.4%'],
		[0.82, 0, '82%'],
		[0, 1, '0.0%'],
		[0, 0, '0%'],
		[0.0004, 1, '<0.1%'],
		[0.0005, 1, '0.1%'],
		[0.001, 1, '0.1%'],
		[0.9996, 1, '>99.9%'],
		[0.9994, 1, '99.9%'],
		[0.004, 0, '<1%'],
		[0.005, 0, '1%'],
		[0.996, 0, '>99%'],
		[1, 1, '100.0%'],
		[1, 0, '100%'],
		[1.5, 1, '150.0%']
	];
	for (const [input, f, expected] of rows) {
		it(`${input} (f=${f}) → ${expected}`, () => {
			expect(formatPercent(input, 'en', f)).toBe(expected);
			expect(formatPercent(input, 'zh', f)).toBe(expected);
		});
	}
	it('defaults to one fraction digit', () => {
		expect(formatPercent(0.374)).toBe('37.4%');
	});
	it('negative ratios are —', () => {
		expect(formatPercent(-0.1)).toBe(DASH);
	});
});

table('formatShare', formatShare, [
	[82, '82%'],
	[1, '1%'],
	[100, '100%'],
	[0, '0%']
]);

describe('formatRate', () => {
	it('wraps the compact number', () => {
		expect(formatRate(1200)).toBe('1.2K/h');
		expect(formatRate(0)).toBe('0/h');
		expect(formatRate(1200, 'en', 'day')).toBe('1.2K/d');
		expect(formatRate({ value: 1200, approx: true })).toBe(`≈${NB}1.2K/h`);
		expect(formatRate(-1)).toBe(DASH);
	});
});

describe('formatDelta', () => {
	table('count', (v, l) => formatDelta(v, l), [
		[1240, '+1,240'],
		[-3, `${MINUS}3`],
		[0, '0'],
		[-0, '0'],
		[1e12, '+1,000,000,000,000']
	]);
	table('percent f=1', (v, l) => formatDelta(v, l, 'percent', 1), [
		[-0.032, `${MINUS}3.2%`],
		[0.032, '+3.2%'],
		[0, '0.0%'],
		[0.00001, '0.0%'],
		[-0.00001, '0.0%']
	]);
	it('compact and percent f=0', () => {
		expect(formatDelta(-1200, 'en', 'compact')).toBe(`${MINUS}1.2K`);
		expect(formatDelta(0.07, 'en', 'percent', 0)).toBe('+7%');
	});
	it('returns — for an unknown kind instead of throwing', () => {
		expect(formatDelta(5, 'en', 'bogus' as never)).toBe(DASH);
	});
	it('never contains a hyphen-minus', () => {
		expect(formatDelta(-12345)).not.toContain('-');
	});
});

describe('formatLatency', () => {
	const rows: [number, string, string][] = [
		[0.8, '0.8 ms', '0.8毫秒'],
		[12, '12 ms', '12毫秒'],
		[1400, '1.4 sec', '1.4秒'],
		[0, '0 ms', '0毫秒'],
		[0.96, '1 ms', '1毫秒'],
		[999, '999 ms', '999毫秒'],
		[999.6, '1 sec', '1秒'], // would print as 1,000 ms
		[1000, '1 sec', '1秒'],
		[125000, '125 sec', '125秒']
	];
	for (const [input, en, zh] of rows) {
		it(`${input} → ${en}`, () => {
			expect(formatLatency(input, 'en')).toBe(en);
			expect(formatLatency(input, 'zh')).toBe(zh);
		});
	}
	it('negative is —; parts split', () => {
		expect(formatLatency(-1)).toBe(DASH);
		expect(formatLatencyParts(1400)).toEqual({ value: '1.4', unit: 'sec' });
		expect(formatLatencyParts(NaN)).toEqual({ value: DASH, unit: '' });
	});
});

describe('formatSpan', () => {
	const rows: [number, string, string][] = [
		[45, '45 sec', '45秒'],
		[12 * 60, '12 min', '12分钟'],
		[5.2 * 3600, '5.2 hr', '5.2小时'],
		[3.1 * 86400, '3.1 days', '3.1天'],
		[0, '0 sec', '0秒'],
		[59, '59 sec', '59秒'],
		[59.6, '1 min', '1分钟'], // 60 sec would be wrong
		[60, '1 min', '1分钟'],
		[3599, '1 hr', '1小时'],
		[3600, '1 hr', '1小时'],
		[172799, '2 days', '2天'],
		[172800, '2 days', '2天'],
		[47 * 3600, '47 hr', '47小时'],
		[9.96 * 3600, '10 hr', '10小时'],
		[1e9, '11,574 days', '11,574天']
	];
	for (const [input, en, zh] of rows) {
		it(`${input} s → ${en}`, () => {
			expect(formatSpan(input, 'en')).toBe(en);
			expect(formatSpan(input, 'zh')).toBe(zh);
		});
	}
	it('negative is —; parts split', () => {
		expect(formatSpan(-1)).toBe(DASH);
		expect(formatSpanParts(5.2 * 3600)).toEqual({ value: '5.2', unit: 'hr' });
		expect(formatSpanParts(5.2 * 3600, 'zh')).toEqual({ value: '5.2', unit: '小时' });
	});
});

describe('formatRelative', () => {
	const now = T;
	const rows: [number, string, string][] = [
		[-3 * 3600, '3 hours ago', '3小时前'],
		[-86400, 'yesterday', '昨天'],
		[5 * 60, 'in 5 minutes', '5分钟后'],
		[-45, '45 seconds ago', '45秒钟前'],
		[-44, 'just now', '刚刚'],
		[0, 'just now', '刚刚'],
		[44, 'just now', '刚刚'],
		[45, 'in 45 seconds', '45秒钟后'],
		[-59, '59 seconds ago', '59秒钟前'],
		[-60, '1 minute ago', '1分钟前'],
		[-90, '1 minute ago', '1分钟前'], // truncated toward zero
		[-3599, '59 minutes ago', '59分钟前'],
		[-7 * 86400, 'last week', '上周'],
		[-30 * 86400, 'last month', '上个月'],
		[-365 * 86400, 'last year', '去年'],
		[-3 * 365 * 86400, '3 years ago', '3年前']
	];
	for (const [delta, en, zh] of rows) {
		it(`Δ${delta} s → ${en}`, () => {
			expect(formatRelative(now + delta, 'en', { now })).toBe(en);
			expect(formatRelative(now + delta, 'zh', { now })).toBe(zh);
		});
	}
	it('takes the translated just_now message', () => {
		expect(formatRelative(now, 'en', { now, justNow: 'Just now!' })).toBe('Just now!');
	});
	it('uses the clock when now is omitted', () => {
		expect(formatRelative(Date.now() / 1000 - 3 * 3600, 'en')).toBe('3 hours ago');
	});
});

describe('time zones', () => {
	it('treats an empty time zone as the default', () => {
		expect(() => formatDateTime(T, 'en', { timeZone: '' })).not.toThrow();
		expect(formatDateTime(T, 'en', { timeZone: '' })).toBe(formatDateTime(T, 'en'));
	});
	it('falls back instead of throwing on an unknown time zone', () => {
		for (let i = 0; i < 2; i++) {
			for (const f of [formatDateTime, formatDate, formatDateFull]) {
				const out = f(T, 'en', { timeZone: 'Not/AZone' });
				expect(out).toContain('2026');
			}
		}
	});
});

describe('dates', () => {
	it('formatDateTime', () => {
		expect(formatDateTime(T, 'en', UTC)).toBe('Oct 22, 2026, 14:05');
		expect(formatDateTime(T, 'zh', UTC)).toBe('2026年10月22日 14:05');
	});
	it('formatDateTime uses h23 at midnight', () => {
		const midnight = Date.UTC(2026, 9, 22, 0, 5) / 1000;
		expect(formatDateTime(midnight, 'en', UTC)).toBe('Oct 22, 2026, 00:05');
	});
	it('formatDateTime honours the time zone', () => {
		expect(formatDateTime(T, 'en', { timeZone: 'Asia/Tokyo' })).toBe('Oct 22, 2026, 23:05');
	});
	it('formatDateFull', () => {
		expect(formatDateFull(T, 'en', UTC)).toBe('Thu, Oct 22, 2026, 14:05 UTC');
		expect(formatDateFull(T, 'zh', UTC)).toBe('2026年10月22日周四 UTC 14:05');
		expect(formatDateFull(T, 'en', { ...UTC, bucket: 'hour' })).toBe(
			'Thu, Oct 22, 2026, 14:05 UTC'
		);
	});
	it('formatDateFull day and week buckets drop the time', () => {
		for (const bucket of ['day', 'week'] as const) {
			expect(formatDateFull(T, 'en', { ...UTC, bucket })).toBe('Thu, Oct 22, 2026');
			expect(formatDateFull(T, 'zh', { ...UTC, bucket })).toBe('2026年10月22日周四');
		}
	});
	it('formatDate', () => {
		expect(formatDate(T, 'en', UTC)).toBe('Oct 22, 2026');
		expect(formatDate(T, 'zh', UTC)).toBe('2026年10月22日');
	});
	it('handles epoch 0 and negative timestamps', () => {
		expect(formatDate(0, 'en', UTC)).toBe('Jan 1, 1970');
		expect(formatDate(-86400, 'en', UTC)).toBe('Dec 31, 1969');
	});
});

table('formatScore', formatScore, [
	[42, '42'],
	[0, '0'],
	[100, '100']
]);

describe('size histogram (§5a)', () => {
	it('labels power-of-2 edges with formatBytes', () => {
		const { edges, labels } = sizeBinEdges(28, 32);
		expect(edges).toEqual([2 ** 28, 2 ** 29, 2 ** 30, 2 ** 31, 2 ** 32]);
		expect(labels).toEqual([
			`256${NB}MiB`,
			`512${NB}MiB`,
			`1.00${NB}GiB`,
			`2.00${NB}GiB`,
			`4.00${NB}GiB`
		]);
	});
	it('starts at 1 byte and spans to PiB', () => {
		const { edges, labels } = sizeBinEdges(0, 50);
		expect(edges).toHaveLength(51);
		expect(labels[0]).toBe(`1${NB}B`);
		expect(labels[10]).toBe(`1.00${NB}KiB`);
		expect(labels[50]).toBe(`1.00${NB}PiB`);
	});
	it('rejects invalid exponents without throwing', () => {
		const bad: [number, number][] = [
			[5, 2],
			[-1, 4],
			[0, 61],
			[1.5, 3],
			[NaN, 3]
		];
		for (const [a, b] of bad) {
			expect(sizeBinEdges(a, b)).toEqual({ edges: [], labels: [] });
		}
	});
	it('labels a bin "lo – hi" with an en dash', () => {
		expect(sizeBinLabel(2 ** 30, 2 ** 32)).toBe(`1.00${NB}GiB${NB}– 4.00${NB}GiB`);
		expect(sizeBinLabel(0, 1024)).toBe(`0${NB}B${NB}– 1.00${NB}KiB`);
	});
	it('keeps empty bins labelled', () => {
		expect(sizeBinLabel(2 ** 20, 2 ** 21)).toContain('1.00');
	});
});

describe('formatter caching', () => {
	it('is stable across repeated calls and locales', () => {
		for (let i = 0; i < 3; i++) {
			expect(formatCount(1284, 'en')).toBe('1,284');
			expect(formatCompact(84200, 'zh')).toBe('8.4万');
		}
	});
});
