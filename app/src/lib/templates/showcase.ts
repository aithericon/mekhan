// Showcase lookup. The Invoice Processing demo lives on disk at
// `demos/invoice-processing/` and is published by the service-side startup
// seeder (`mekhan_service::demos::seed_all`, gated by `MEKHAN__DEMOS__SEED`)
// under a stable template id. The frontend just resolves the id — no
// inline graph data, no `findOrCreate`-style write path. If the seeded
// row is absent, the demo button surfaces an actionable hint pointing
// at the env flag instead of silently no-op-ing.
import type { Template } from '$lib/api/client';
import { getTemplate } from '$lib/api/client';

/// Stable id baked into `demos/invoice-processing/.mekhan.json::templateId`.
/// Single source of truth — both the seeder and this lookup agree on it.
export const SHOWCASE_TEMPLATE_ID = '00000000-0000-0000-0000-000000000001';
export const SHOWCASE_TEMPLATE_NAME = 'Invoice Processing Demo';

/// Look up the seeded demo by its stable id. Returns `null` if the seeder
/// hasn't run (rather than throwing) so callers can decide between
/// "show a hint" and "fall back to something else".
export async function findShowcaseTemplate(): Promise<Template | null> {
	try {
		return await getTemplate(SHOWCASE_TEMPLATE_ID);
	} catch (e) {
		// `getTemplate` rejects with a Response or Error on 404. Treat any
		// failure as "not seeded yet" — the deeper diagnostic is already
		// in the service log.
		void e;
		return null;
	}
}
