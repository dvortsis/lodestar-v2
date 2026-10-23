import { describe, expect, it } from 'vitest';
import { checkPassword, isBlocking, MAX_PASSWORD_BYTES, passwordChars } from './password';

describe('checkPassword', () => {
	it('blocks fewer than 10 characters, counting code points like the server', () => {
		expect(checkPassword('')).toBe('too_short');
		expect(checkPassword('abcdefgh9')).toBe('too_short');
		// Nine emoji are 18 UTF-16 units but nine characters.
		expect(checkPassword('😀'.repeat(9))).toBe('too_short');
		expect(passwordChars('😀'.repeat(9))).toBe(9);
		expect(isBlocking(checkPassword('😀'.repeat(10)))).toBe(false);
	});

	it('blocks over 1,024 bytes of UTF-8, not 1,024 characters', () => {
		expect(isBlocking(checkPassword('a1b2c3d4e5'.repeat(102)))).toBe(false); // 1,020 bytes
		expect(checkPassword('é'.repeat(MAX_PASSWORD_BYTES / 2 + 1))).toBe('too_long');
	});

	it('never blocks a long passphrase, whatever it is made of', () => {
		expect(checkPassword('correct horse battery staple')).toBe('strong');
		expect(checkPassword('lowercase words only here')).toBe('strong');
		expect(checkPassword('密码是一个很长的句子而且很难猜到')).toBe('strong');
	});

	it('warns, without blocking, on repeats and runs', () => {
		for (const pw of [
			'aaaaaaaaaa',
			'abababababab',
			'1234567890',
			'abcdefghijkl',
			'9876543210',
			'Password123!',
			'qwertyuiop',
			'admin2026!!'
		]) {
			expect(checkPassword(pw), pw).toBe('weak');
			expect(isBlocking(checkPassword(pw))).toBe(false);
		}
	});

	it('rates short mixed passwords fair or strong', () => {
		expect(checkPassword('tulipfield')).toBe('fair');
		expect(checkPassword('Tulip-field9')).toBe('strong');
		expect(checkPassword('Tulipfield')).toBe('fair');
	});
});
