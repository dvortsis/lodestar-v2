import { error } from '@sveltejs/kit';
import { parseId } from '$lib/features/alerts/api';

// A saved search's id is a positive integer; anything else is a 404 (W9's error page).
export function load({ params }: { params: { id: string } }) {
	const id = parseId(params.id);
	if (id === null) error(404, 'Not found');
	return { id };
}
