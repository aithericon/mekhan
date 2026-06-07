import { redirect } from '@sveltejs/kit';

// /models has no content of its own — land on the Set tab (the curated model
// set is the conceptual centre of the pool).
export const load = () => {
	redirect(307, '/models/set');
};
