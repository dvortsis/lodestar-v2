import { describe, expect, it } from 'vitest';
import { asValues, merge, resolve } from './merge';
import { sv, values } from './testing';
import type { SettingValue } from './types';
import {
	differs,
	dirtyNames,
	equal,
	fromValues,
	isDirty,
	RESET,
	stage,
	type SectionState,
	type Staged
} from './values';

const start = (fields: Record<string, SettingValue>, revision = 3) =>
	fromValues(values('crawler', revision, fields));

describe('the merge table', () => {
	const s0 = () => start({ a: sv('x'), b: sv(1, 'db'), c: sv(true) });

	it('takes theirs silently for an untouched field', () => {
		const out = merge(
			s0(),
			values('crawler', 4, { a: sv('y', 'db'), b: sv(1, 'db'), c: sv(true) })
		);
		expect(out.state.fields.a).toEqual({ saved: sv('y', 'db') });
		expect(out.updated).toEqual(['a']);
		expect(out.conflicts).toEqual([]);
		expect(out.state.revision).toBe(4);
	});

	it('keeps mine when only I changed it, rebased on theirs', () => {
		const s = stage(s0(), 'a', 'mine');
		const out = merge(s, values('crawler', 4, { a: sv('x'), b: sv(2, 'db'), c: sv(true) }));
		expect(out.state.fields.a).toMatchObject({ base: 'x', mine: 'mine' });
		expect(isDirty(out.state.fields.a!)).toBe(true);
		expect(out.conflicts).toEqual([]);
	});

	it('is clean when both changed it to the same value', () => {
		const s = stage(s0(), 'a', 'same');
		const out = merge(
			s,
			values('crawler', 4, { a: sv('same', 'db'), b: sv(1, 'db'), c: sv(true) })
		);
		expect(out.state.fields.a).toEqual({ saved: sv('same', 'db') });
		expect(out.conflicts).toEqual([]);
	});

	it('is a conflict when both changed it differently, until resolved', () => {
		const s = stage(s0(), 'a', 'mine');
		const theirs = values('crawler', 4, { a: sv('theirs', 'db'), b: sv(1, 'db'), c: sv(true) });
		const out = merge(s, theirs);
		expect(out.conflicts).toEqual(['a']);
		expect(out.state.fields.a!.conflict).toEqual({ mine: 'mine', theirs: 'theirs' });

		const kept = resolve(out.state, 'a', 'mine');
		expect(kept.fields.a).toMatchObject({ base: 'theirs', mine: 'mine' });
		expect(kept.fields.a!.conflict).toBeUndefined();
		expect(dirtyNames(kept)).toEqual(['a']);

		const taken = resolve(out.state, 'a', 'theirs');
		expect(taken.fields.a).toEqual({ saved: sv('theirs', 'db') });
		expect(dirtyNames(taken)).toEqual([]);
	});

	it('treats a reset as matching any value that is no longer saved', () => {
		const s = stage(s0(), 'b', RESET);
		const reset = merge(s, values('crawler', 4, { a: sv('x'), b: sv(5, 'file'), c: sv(true) }));
		expect(reset.conflicts).toEqual([]);
		expect(dirtyNames(reset.state)).toEqual([]);
		const changed = merge(s, values('crawler', 4, { a: sv('x'), b: sv(2, 'db'), c: sv(true) }));
		expect(changed.conflicts).toEqual(['b']);
		expect(changed.state.fields.b!.conflict).toEqual({ mine: RESET, theirs: 2 });
	});

	it('drops my edit when the field is now locked, and says so', () => {
		const s = stage(s0(), 'a', 'mine');
		const out = merge(
			s,
			values('crawler', 4, {
				a: sv('env', 'env', { env: 'LODESTAR_A', read_only: true }),
				b: sv(1, 'db'),
				c: sv(true)
			})
		);
		expect(out.locked).toEqual(['a']);
		expect(out.state.fields.a).toMatchObject({ note: 'locked' });
		expect(out.state.fields.a!.mine).toBeUndefined();
	});

	it('drops a field the server no longer has', () => {
		const s = stage(s0(), 'c', false);
		const out = merge(s, values('crawler', 4, { a: sv('x'), b: sv(1, 'db') }));
		expect(out.removed).toEqual(['c']);
		expect(out.state.fields.c).toBeUndefined();
	});

	it('takes a field the server added', () => {
		const out = merge(
			s0(),
			values('crawler', 4, { a: sv('x'), b: sv(1, 'db'), c: sv(true), d: sv(0) })
		);
		expect(out.state.fields.d).toEqual({ saved: sv(0) });
	});

	it('keeps an existing conflict when the same theirs comes again, and updates it otherwise', () => {
		const s = stage(s0(), 'a', 'mine');
		const t1 = values('crawler', 4, { a: sv('t1', 'db'), b: sv(1, 'db'), c: sv(true) });
		const once = merge(s, t1).state;
		expect(merge(once, t1).state).toEqual(once);
		const t2 = values('crawler', 5, { a: sv('t2', 'db'), b: sv(1, 'db'), c: sv(true) });
		expect(merge(once, t2).state.fields.a!.conflict).toEqual({ mine: 'mine', theirs: 't2' });
		const t3 = values('crawler', 6, { a: sv('mine', 'db'), b: sv(1, 'db'), c: sv(true) });
		expect(merge(once, t3).state.fields.a).toEqual({ saved: sv('mine', 'db') });
	});

	it('reads its saved values back as section values', () => {
		const v = values('crawler', 3, { a: sv('x') }, ['a']);
		expect(asValues(fromValues(v))).toEqual(v);
	});
});

// A seeded PRNG (mulberry32): the same cases on every run, no new dependency.
function prng(seed: number) {
	let a = seed >>> 0;
	return () => {
		a = (a + 0x6d2b79f5) >>> 0;
		let t = a;
		t = Math.imul(t ^ (t >>> 15), t | 1);
		t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
		return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
	};
}

describe('the merge, as a property', () => {
	const r = prng(0x5e7711);
	const pick = <T>(xs: readonly T[]): T => xs[Math.floor(r() * xs.length)]!;
	const POOL: unknown[] = [
		0,
		1,
		-0,
		'a',
		'b',
		true,
		false,
		null,
		[1, 2],
		[2, 1],
		{ x: 1 },
		{ x: 1, y: [1] },
		{ y: [1], x: 1 }
	];
	const value = () => structuredClone(pick(POOL));
	const source = () => pick(['default', 'file', 'db'] as const);

	type Case = { s: SectionState; theirs: ReturnType<typeof values>; touched: Map<string, Staged> };

	const locked = (v: SettingValue) => v.source === 'env' || v.read_only;

	function gen(): Case {
		const n = 1 + Math.floor(r() * 6);
		const fields: Record<string, SettingValue> = {};
		for (let i = 0; i < n; i++) {
			// Some start locked (env-set or read-only): they never take an edit.
			const roll = r();
			fields[`f${i}`] =
				roll < 0.1
					? sv(value(), 'env', { env: 'LODESTAR_F', read_only: true })
					: roll < 0.15
						? sv(value(), 'default', { read_only: true })
						: sv(value(), source());
		}
		let s = start(fields);
		const touched = new Map<string, Staged>();
		for (const name of Object.keys(fields)) {
			if (r() < 0.5) continue;
			const mine = r() < 0.2 ? RESET : value();
			s = stage(s, name, mine);
			if (s.fields[name]!.mine !== undefined) touched.set(name, s.fields[name]!.mine);
			else if (!locked(fields[name]!)) expect(differs(mine, fields[name]!)).toBe(false);
		}
		const tf: Record<string, SettingValue> = {};
		for (const [name, f] of Object.entries(fields)) {
			const roll = r();
			if (roll < 0.08) continue; // removed
			if (roll < 0.16) tf[name] = sv(value(), 'env', { env: 'LODESTAR_X', read_only: true });
			else if (roll < 0.55)
				tf[name] = f; // unchanged
			else tf[name] = sv(value(), source());
		}
		if (r() < 0.2) tf.added = sv(value());
		return { s, theirs: values('crawler', 3 + Math.floor(r() * 5), tf), touched };
	}

	const matches = (mine: Staged, t: SettingValue) =>
		mine === RESET ? t.source !== 'db' : equal(mine, t.value);

	it('holds over 2,000 seeded cases', () => {
		const seen = { conflicts: 0, locked: 0, removed: 0, kept: 0, resets: 0, lockedStart: 0 };
		for (let i = 0; i < 2000; i++) {
			const { s, theirs, touched } = gen();
			const out = merge(s, theirs);
			seen.conflicts += out.conflicts.length;
			seen.locked += out.locked.length;
			seen.removed += out.removed.length;
			seen.resets += [...touched.values()].filter((v) => v === RESET).length;
			seen.lockedStart += Object.values(s.fields).filter((f) => locked(f.saved)).length;
			for (const [name, f] of Object.entries(s.fields)) {
				if (locked(f.saved)) expect(touched.has(name), `case ${i}`).toBe(false);
			}
			seen.kept += Object.values(out.state.fields).filter((f) => f.mine && !f.conflict).length;
			const st = out.state;
			const ctx = `case ${i}`;
			// (f) the revision is theirs
			expect(st.revision, ctx).toBe(theirs.revision);
			// removed fields are gone, every field of theirs is present and saved as theirs
			for (const name of Object.keys(s.fields)) {
				if (!(name in theirs.fields)) expect(st.fields[name], ctx).toBeUndefined();
			}
			for (const [name, t] of Object.entries(theirs.fields)) {
				const f = st.fields[name]!;
				expect(f.saved, ctx).toEqual(t);
				const mine = touched.get(name);
				const base = s.fields[name]?.base;
				if (mine === undefined) {
					// (a) fields you didn't touch end equal to theirs
					expect(f.mine, ctx).toBeUndefined();
					continue;
				}
				if (locked(t)) {
					expect(f.mine, ctx).toBeUndefined();
					continue;
				}
				const theirsChanged = !equal(base, t.value);
				if (!theirsChanged) {
					// (b) fields only you touched keep yours
					expect(f.conflict, ctx).toBeUndefined();
					if (f.mine !== undefined) expect(f.mine, ctx).toBe(mine);
					else expect(matches(mine, t), ctx).toBe(true);
				}
				// (c) a conflict iff both changed, to different values
				const conflict = theirsChanged && !matches(mine, t);
				expect(f.conflict !== undefined, ctx).toBe(conflict);
			}
			expect(out.conflicts.sort(), ctx).toEqual(
				Object.keys(st.fields)
					.filter((n) => st.fields[n]!.conflict)
					.sort()
			);
			// (e) merging twice with the same theirs changes nothing
			expect(merge(st, theirs).state, ctx).toEqual(st);
			// (d) after Keep mine on every conflict, dirty = fields where mine ≠ theirs
			let kept = st;
			for (const name of out.conflicts) kept = resolve(kept, name, 'mine');
			// Expected from the generator's inputs, not from the merged state.
			const expectDirty = [...touched].filter(([name, mine]) => {
				const t = theirs.fields[name];
				return t !== undefined && !locked(t) && !matches(mine, t);
			});
			expect(dirtyNames(kept).sort(), ctx).toEqual(expectDirty.map(([n]) => n).sort());
			for (const [name, mine] of expectDirty) expect(kept.fields[name]!.mine, ctx).toBe(mine);
			// …and Use theirs on every conflict leaves none of them dirty
			let taken = st;
			for (const name of out.conflicts) taken = resolve(taken, name, 'theirs');
			for (const name of out.conflicts) expect(isDirty(taken.fields[name]!), ctx).toBe(false);
		}
		// The generator reaches every branch of the table, often.
		for (const [k, n] of Object.entries(seen)) expect(n, k).toBeGreaterThan(100);
	});
});
