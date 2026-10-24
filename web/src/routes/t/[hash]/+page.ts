import { error } from '@sveltejs/kit';

const HASH = /^[0-9a-f]{40}$/;

export const load = ({ params }) => {
	const hash = params.hash.toLowerCase();
	if (!HASH.test(hash)) error(404, 'Not found');
	return { hash };
};
