import { redirect } from '@sveltejs/kit';

// The inventory is no longer a standalone page — it's the Copies tab of the
// unified Data browser. Old links/bookmarks land there.
export const load = () => {
	redirect(308, '/data?tab=copies');
};
