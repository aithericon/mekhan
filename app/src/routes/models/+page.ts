import { redirect } from '@sveltejs/kit';

// /models has no content of its own — land on the Engines tab.
export const load = () => {
	redirect(307, '/models/engines');
};
