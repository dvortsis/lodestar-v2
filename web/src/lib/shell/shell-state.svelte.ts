// Shell UI state: whether the command palette is open.
let paletteOpen = $state(false);

export const shell = {
	get paletteOpen() {
		return paletteOpen;
	},
	set paletteOpen(v: boolean) {
		paletteOpen = v;
	},
	togglePalette() {
		paletteOpen = !paletteOpen;
	}
};
