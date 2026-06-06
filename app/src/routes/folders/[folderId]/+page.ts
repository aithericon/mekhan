import { redirect } from '@sveltejs/kit';
import type { PageLoad } from './$types';

// Bare `/folders/[folderId]` has no content of its own — the tab subroutes
// (`templates`, `api`) and the gear-linked `settings` are the actual views.
// Land on Templates by default.
export const load: PageLoad = ({ params }) => {
	throw redirect(307, `/folders/${params.folderId}/templates`);
};
