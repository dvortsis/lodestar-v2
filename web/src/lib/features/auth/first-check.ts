// The search page's sign-in check (X-first-run-search-gate), kept free of imports so the bare first
// load gains no chunk: one `GET /auth`, and the module that navigates (`first-run.ts`) loads only
// when the browser has to leave. A failed or unreadable check leaves the page where it is; the
// page's own requests still meet the client's 401 handler. `stillHere` is asked before leaving, so
// an answer that comes after the visitor has moved on never pulls them back.
type Status = { logged_in: boolean; notice_required: boolean; setup_required: boolean };

export async function checkFirstRun(stillHere: () => boolean): Promise<void> {
	let status: Status;
	try {
		const res = await fetch('/api/v1/auth', {
			credentials: 'same-origin',
			headers: { accept: 'application/json' }
		});
		if (!res.ok) return;
		status = (await res.json()) as Status;
	} catch {
		return;
	}
	if (status.logged_in && !status.notice_required && !status.setup_required) return;
	if (!stillHere()) return;
	const { leaveForFirstRun } = await import('./first-run');
	if (stillHere()) await leaveForFirstRun(status);
}
