// The words for each failure. Only fixed messages: the server's own text is never shown, so
// nothing a request carried (the setup token above all) can come back on screen.
import { m } from '$lib/paraglide/messages.js';
import type { Failure } from './machine';
import { MAX_PASSWORD_BYTES, MIN_PASSWORD_CHARS } from './password';
import { TOKEN_HINT } from './token';

export function failureMessage(f: Failure): string {
	switch (f.kind) {
		case 'invalid':
			switch (f.reason) {
				case 'token_invalid':
					return m.auth_error_token_invalid({ format: TOKEN_HINT });
				case 'too_long':
					return m.auth_error_too_long({ max: String(MAX_PASSWORD_BYTES) });
				case 'mismatch':
					return m.auth_error_mismatch();
				case 'required':
					return m.auth_error_required();
				default:
					return m.auth_error_too_short({ min: String(MIN_PASSWORD_CHARS) });
			}
		case 'bad_token':
			return m.auth_error_bad_token();
		case 'bad_password':
			return m.auth_error_bad_password();
		case 'weak_password':
			return m.auth_error_too_short({ min: String(MIN_PASSWORD_CHARS) });
		case 'network':
			return m.auth_error_network();
		case 'rate_limited':
			return m.auth_error_rate_limited();
		default:
			return m.auth_error_server();
	}
}
