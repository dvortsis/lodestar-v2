import { describe, expect, it } from 'vitest';
import {
	automaticNumber,
	automaticValue,
	displayValue,
	estimatePart,
	optionLabel
} from './display';
import { spec } from './testing/values';
import type { FieldState } from './types';

const withEstimates = (estimates: { name: string; value: unknown }[]) =>
	({ preview: { restartRequired: false, estimates, warnings: [] } }) as unknown as FieldState;

describe('displayValue', () => {
	it('reads each kind of value as a phrase', () => {
		const n = spec('crawler.timeout_secs', { kind: 'integer', unit: 'seconds' });
		expect(displayValue(n, 1200)).toBe('1,200 seconds');
		expect(displayValue(spec('s.f', { kind: 'integer' }), 3)).toBe('3');
		expect(displayValue(spec('s.f', { kind: 'boolean' }), true)).toBe('On');
		expect(displayValue(spec('s.f', { kind: 'boolean' }), false)).toBe('Off');
		expect(displayValue(spec('s.f', { kind: 'enum', enum: ['low'] }), 'low')).toBe('low');
		expect(displayValue(spec('s.f'), '0.0.0.0:8090')).toBe('0.0.0.0:8090');
		expect(displayValue(spec('s.f', { kind: 'string-list' }), ['a', 'b'])).toBe('a, b');
		expect(displayValue(spec('s.f', { kind: 'map' }), { a: 1 })).toBe('{"a":1}');
	});

	it('shows nothing as a dash, never as 0', () => {
		for (const v of [null, undefined, '']) expect(displayValue(spec('s.f'), v)).toBe('—');
	});

	it('labels an enum value by its message, falling back to the value', () => {
		expect(optionLabel(spec('crawler.intensity', { kind: 'enum' }), 'aggressive')).toBe(
			'aggressive'
		);
	});
});

describe('estimatePart', () => {
	it('formats sockets as a count, memory through formatBytes, and the URL apart', () => {
		expect(estimatePart({ name: 'sockets', value: 1000 })).toEqual({
			text: '≈ 1,000 connections',
			testid: 'estimate-sockets'
		});
		// MiB × 1,048,576 bytes: a unit change, not a computation.
		expect(estimatePart({ name: 'memory_mib', value: 512 })).toEqual({
			text: '≈ 512 MiB of memory',
			testid: 'estimate-memory'
		});
		expect(estimatePart({ name: 'url', value: 'http://192.168.1.5:9000/' })).toEqual({
			text: 'Address after a restart:',
			mono: 'http://192.168.1.5:9000/',
			testid: 'estimate-url'
		});
	});

	it('leaves out an estimate it doesn’t know, or of the wrong type', () => {
		expect(estimatePart({ name: 'carbon', value: 3 })).toBeUndefined();
		expect(estimatePart({ name: 'sockets', value: '400' })).toBeUndefined();
		expect(estimatePart({ name: 'url', value: 9000 })).toBeUndefined();
	});
});

describe('the automatic value', () => {
	it('is the field’s one estimate', () => {
		const f = withEstimates([{ name: 'sockets', value: 400 }]);
		expect(automaticValue(f)).toBe('400');
		expect(automaticNumber(f)).toBe(400);
	});

	it('is unknown with no estimate, several, or one that isn’t a number', () => {
		for (const f of [
			withEstimates([]),
			withEstimates([
				{ name: 'sockets', value: 400 },
				{ name: 'memory_mib', value: 8 }
			]),
			withEstimates([{ name: 'url', value: 'http://x/' }]),
			{} as FieldState
		]) {
			expect(automaticValue(f)).toBeUndefined();
			expect(automaticNumber(f)).toBeUndefined();
		}
	});
});
