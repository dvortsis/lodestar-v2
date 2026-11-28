// A reactive stand-in for the results store in the toolbar's tests: the hidden count follows
// `query`, so a test changes it while the toolbar is mounted, as a new query does.
export const fakeResults = $state({
	status: 'ready',
	hits: [] as unknown[],
	total: 0,
	totalApprox: false,
	refining: false,
	query: null as { ast: unknown; hidden: boolean } | null,
	grouped: {} as Record<string, unknown>
});
