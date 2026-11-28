// A reactive stand-in for the search state's `q`, for Rescue's spec.
let q = $state('');
export const rescueSearch = {
	get q() {
		return q;
	},
	setText: (t: string) => (q = t),
	submit: async () => {}
};
export const setQ = (next: string) => (q = next);
