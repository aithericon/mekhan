/**
 * Tests for the Fleet → Interfaces catalog's catalog derivations (docs/29 P5).
 *
 * A model-server runner self-reports its loaded LLM models on the SAME
 * interface catalog ROS runners use for topics/services/actions
 * (`RunnerInterfaceCatalog.models: ModelEntry[]`). The UI surfaces those as a
 * first-class "Models" section, and — crucially — a model-only runner must NOT
 * be shown the "No catalog reported yet" empty state.
 *
 * Following this suite's convention (grouping.test.ts, SchemaValueView.test.ts
 * et al.), the component's render gating lives in pure exported helpers
 * (`./interfaces-catalog`) which we unit-test directly rather than mounting a
 * DOM — the vitest config compiles Svelte in SSR mode, so `mount` is
 * unavailable, and the codebase tests logic, not markup.
 */
import { describe, it, expect } from 'vitest';
import type { ModelEntry, RunnerInterfaceCatalog } from '$lib/api/runners';
import {
	interfaceGroups,
	catalogModels,
	rosEntryCount,
	totalCatalogEntries,
	modelCapacityLabel
} from './interfaces-catalog';

const base: ModelEntry = { model_id: 'llama3', kind: 'base', max_num_seqs: 4 };
const lora: ModelEntry = {
	model_id: 'med-lora',
	kind: 'lora',
	base: 'llama3',
	source_uri: 'hf://med'
};

describe('interfaces-catalog derivations', () => {
	describe('catalogModels', () => {
		it('returns the reported models, in order', () => {
			const cat: RunnerInterfaceCatalog = { models: [base, lora] };
			expect(catalogModels(cat)).toEqual([base, lora]);
		});

		it('defaults to an empty list when absent or catalog is null', () => {
			expect(catalogModels({})).toEqual([]);
			expect(catalogModels(null)).toEqual([]);
			expect(catalogModels(undefined)).toEqual([]);
		});
	});

	describe('totalCatalogEntries — empty-state gate', () => {
		it('counts models so a model-only runner is NOT empty', () => {
			const cat: RunnerInterfaceCatalog = { models: [base, lora] };
			const groups = interfaceGroups(cat);
			const models = catalogModels(cat);
			expect(rosEntryCount(groups)).toBe(0); // no ROS ifaces
			expect(totalCatalogEntries(groups, models)).toBe(2); // but two models ⇒ not empty
		});

		it('is zero when the runner reports nothing at all', () => {
			const cat: RunnerInterfaceCatalog = { models: [], topics: [], services: [], actions: [] };
			const groups = interfaceGroups(cat);
			expect(totalCatalogEntries(groups, catalogModels(cat))).toBe(0);
		});

		it('counts ROS interfaces alongside models', () => {
			const cat: RunnerInterfaceCatalog = {
				topics: [{ name: '/cmd_vel', type: 'geometry_msgs/Twist' }],
				models: [base]
			};
			const groups = interfaceGroups(cat);
			expect(rosEntryCount(groups)).toBe(1);
			expect(totalCatalogEntries(groups, catalogModels(cat))).toBe(2);
		});
	});

	describe('modelCapacityLabel', () => {
		it('renders C=<max_num_seqs> for a base model', () => {
			expect(modelCapacityLabel(base)).toBe('C=4');
		});

		it('is null for a LoRA (capacity is per-base, shared)', () => {
			expect(modelCapacityLabel(lora)).toBeNull();
		});

		it('is null for a base missing max_num_seqs', () => {
			expect(modelCapacityLabel({ model_id: 'x', kind: 'base' })).toBeNull();
		});
	});

	describe('interfaceGroups', () => {
		it('returns the three groups in render order', () => {
			expect(interfaceGroups({}).map((g) => g.label)).toEqual(['Topics', 'Services', 'Actions']);
		});

		it('returns [] for a null catalog', () => {
			expect(interfaceGroups(null)).toEqual([]);
		});
	});
});
