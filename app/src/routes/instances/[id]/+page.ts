import { redirect } from '@sveltejs/kit';
import type { PageLoad } from './$types';

// Bare `/instances/[id]` has no content of its own — the four tab subroutes
// (`process`, `workflow`, `steps`, `petri-net`) are the actual views. Land on
// Process by default; users navigate from there.
export const load: PageLoad = ({ params }) => {
	throw redirect(307, `/instances/${params.id}/process`);
};
