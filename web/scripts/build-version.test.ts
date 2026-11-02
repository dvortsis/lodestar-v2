// @vitest-environment node
import { describe, expect, it } from 'vitest';
import { buildVersion } from './build-version.mjs';

describe('buildVersion', () => {
	it('is non-empty and stable across calls', () => {
		expect(buildVersion({})).not.toBe('');
		expect(buildVersion({})).toBe(buildVersion({}));
	});
	it('adds the short commit SHA when given', () => {
		const sha = '0123456789abcdef0123456789abcdef01234567';
		expect(buildVersion({ GITHUB_SHA: sha })).toBe(`${buildVersion({})}-0123456`);
	});
	it('ignores an empty SHA', () => {
		expect(buildVersion({ GITHUB_SHA: ' ' })).toBe(buildVersion({}));
	});
	it('lets LODESTAR_BUILD_VERSION override the SHA', () => {
		expect(buildVersion({ GITHUB_SHA: 'abcdef0123', LODESTAR_BUILD_VERSION: '0000000' })).toBe(
			'0000000'
		);
		expect(buildVersion({ LODESTAR_BUILD_VERSION: ' ' })).toBe(buildVersion({}));
	});
});
