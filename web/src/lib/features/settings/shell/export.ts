import {
	abortError,
	ApiError,
	errorFromResponse,
	handleUnauthorized,
	sessionOk
} from '$lib/api/client';

/** The backend owns export redaction and the portable-file format. Never serialize form values. */
export async function requestSettingsExport(
	preferences: boolean,
	signal: AbortSignal
): Promise<Blob> {
	if (signal.aborted) throw abortError();
	const path = '/settings/export';
	const response = await fetch(`/api/v1${path}?preferences=${preferences}`, {
		credentials: 'same-origin',
		headers: { Accept: 'application/toml' },
		signal
	});
	if (signal.aborted) throw abortError();
	if (!response.ok) {
		const error = await errorFromResponse(response);
		if (signal.aborted) throw abortError();
		handleUnauthorized(error, path);
		throw error;
	}
	// A proxy/login page or JSON error returned with 200 must not become a settings backup.
	if (
		response.headers.get('content-type')?.split(';')[0]?.trim().toLowerCase() !== 'application/toml'
	)
		throw new ApiError(response.status, 'bad_response', 'Expected a settings export');
	const file = await response.blob();
	if (signal.aborted) throw abortError();
	sessionOk(path);
	return file;
}
