// Shared state for the registered data types on the /data Entries surface.
// One instance per page mount (same pattern as EntriesQueryState): the rail's
// Data-types section and Schemas facet group render from it, while every
// compile site (EntriesTab, FacetGroup, saveCurrent) resolves `datatype:`
// terms through the bound `resolveDigests`.
import {
	listDataTypes,
	createDataType,
	updateDataType,
	deleteDataType,
	type CatalogueDataType,
	type DataTypePromote,
	type DataTypeUpdate
} from '$lib/api/data';
import type { DatatypeResolver } from './query-language';

export class DataTypesState {
	list = $state<CatalogueDataType[]>([]);
	loading = $state(true);
	error = $state<string | null>(null);

	/** digest → owning type (server enforces a digest belongs to ≤1 type). */
	byDigest = $derived(
		new Map(this.list.flatMap((t) => t.digests.map((d) => [d, t] as const)))
	);
	/** Registered names, for validateTerms' unknown-datatype warning. */
	names = $derived(new Set(this.list.map((t) => t.name)));

	/** Bound so it can be handed to compileQuery as a bare function. Unknown
	 *  name → undefined (compile fails closed; matches nothing). */
	resolveDigests: DatatypeResolver = (name) =>
		this.list.find((t) => t.name === name)?.digests;

	async load() {
		this.loading = true;
		try {
			this.list = await listDataTypes();
			this.error = null;
		} catch (e) {
			// Keep the last good list — a refresh failure shouldn't blank the rail.
			this.error = e instanceof Error ? e.message : 'Failed to load data types';
		} finally {
			this.loading = false;
		}
	}

	// Mutations reload (list/get carry server-derived columns + live
	// entry_count — never patch those locally) and rethrow so dialogs can
	// surface 404/409/422 themselves.
	async promote(body: DataTypePromote): Promise<CatalogueDataType> {
		const created = await createDataType(body);
		await this.load();
		return created;
	}

	async update(id: string, body: DataTypeUpdate): Promise<CatalogueDataType> {
		const updated = await updateDataType(id, body);
		await this.load();
		return updated;
	}

	async remove(id: string): Promise<void> {
		await deleteDataType(id);
		await this.load();
	}
}
