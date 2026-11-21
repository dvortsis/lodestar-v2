// A theme's display name (X-labels), for Appearance.

/** The theme's own display name (`riverstone` → "Riverstone", `ayu-dark` → "Ayu"). */
export function themeLabel(id: string, themes: readonly { id: string; family: string }[]): string {
	const family = id.replace(/-(light|dark)$/, '');
	if (family === 'riverstone') return 'Riverstone';
	return themes.find((t) => t.id.replace(/-(light|dark)$/, '') === family)?.family ?? id;
}
