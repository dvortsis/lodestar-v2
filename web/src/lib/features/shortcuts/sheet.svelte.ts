// Whether the shortcut sheet is open. Its own file so the shell's boot code can open the sheet
// without importing the component.
export const sheet = $state({ open: false });
