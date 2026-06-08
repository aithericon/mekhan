import { redirect } from '@sveltejs/kit';

// The catalogue is no longer a standalone page — it's the Entries tab of the
// unified Data browser. Old links/bookmarks land there. (Lineage + provenance
// detail routes still live under /catalogue/* and are reached from artifacts.)
export const load = () => {
	redirect(308, '/data?tab=entries');
};
