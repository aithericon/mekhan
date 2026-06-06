/**
 * Pure derivations behind the Fleet → Interfaces catalog
 * (`InterfacesCatalog.svelte`).
 *
 * The component is a thin shell over the runner data layer; the interesting
 * logic is "given a runner's self-reported catalog, what sections does the
 * panel render?". That includes the model-server case (docs/29 P5): a runner's
 * loaded LLM models ride the SAME `RunnerInterfaceCatalog` as ROS
 * topics/services/actions and surface as a first-class Models section, so a
 * model-only runner must NOT fall into the "no catalog reported" empty state.
 *
 * Following this suite's convention (grouping.test.ts et al.) these helpers are
 * exported and unit-tested directly, so the test doesn't need a DOM mount.
 */
import type { InterfaceEntry, ModelEntry, RunnerInterfaceCatalog } from '$lib/api/runners';

/** One ROS interface group (topics/services/actions) in render order. */
export interface InterfaceGroup {
	label: string;
	entries: InterfaceEntry[];
}

/** The three ROS interface groups, in render order, for a catalog. */
export function interfaceGroups(catalog: RunnerInterfaceCatalog | null | undefined): InterfaceGroup[] {
	if (!catalog) return [];
	return [
		{ label: 'Topics', entries: catalog.topics ?? [] },
		{ label: 'Services', entries: catalog.services ?? [] },
		{ label: 'Actions', entries: catalog.actions ?? [] }
	];
}

/** The loaded LLM models a model-server runner self-reports. */
export function catalogModels(catalog: RunnerInterfaceCatalog | null | undefined): ModelEntry[] {
	return catalog?.models ?? [];
}

/** Count of ROS interface entries across topics/services/actions. */
export function rosEntryCount(groups: InterfaceGroup[]): number {
	return groups.reduce((n, g) => n + g.entries.length, 0);
}

/**
 * Total "things this runner reported" — ROS interfaces PLUS loaded models.
 * Drives the empty state: `0` ⇒ render "No catalog reported yet". Counting
 * models here is what lets a model-only runner avoid the empty state.
 */
export function totalCatalogEntries(groups: InterfaceGroup[], models: ModelEntry[]): number {
	return rosEntryCount(groups) + models.length;
}

/** The capacity cell for a model row: `C=<max_num_seqs>` for a base, else null. */
export function modelCapacityLabel(model: ModelEntry): string | null {
	if (model.kind === 'base' && model.max_num_seqs != null) {
		return `C=${model.max_num_seqs}`;
	}
	return null;
}
