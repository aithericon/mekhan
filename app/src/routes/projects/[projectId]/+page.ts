import { redirect } from '@sveltejs/kit';
import type { PageLoad } from './$types';

// Bare `/projects/[projectId]` has no content of its own — the tab subroutes
// (`templates`, `api`) and the gear-linked `settings` are the actual views.
// Land on Templates by default.
export const load: PageLoad = ({ params }) => {
	throw redirect(307, `/projects/${params.projectId}/templates`);
};
