/**
 * Tests for the internal model-pool picker's filter rule.
 *
 * Following this codebase's convention (SchemaValueView.test.ts et al.), we test
 * the pure exported helper that drives the component rather than mounting a DOM:
 * `availableModelIds` is the AND-gate filter the <Select> renders from. The
 * load-on-mount path is a thin `listLoadedModels()` call whose result feeds this
 * helper, so exercising the helper covers the offered-set behaviour.
 *
 * Cases:
 *   1. Only `available === true` models are offered (loaded-but-unserved hidden).
 *   2. None available → empty list (the disabled/empty-state path).
 *   3. Order + ids preserved; LoRA + base rows both pass when available.
 */
import { describe, it, expect } from 'vitest';
import { availableModelIds } from './model-pool';
import type { ModelSetView } from '$lib/api/models';

function row(over: Partial<ModelSetView>): ModelSetView {
	return {
		model_id: 'm',
		available: false,
		state: 'unloaded',
		replicas: 0,
		serving_runners: 0,
		...over
	};
}

describe('availableModelIds', () => {
	it('offers only AND-gate-available models, hiding loaded-but-unserved', () => {
		const models: ModelSetView[] = [
			// loaded AND served → available
			row({ model_id: 'llama3', available: true, state: 'loaded', serving_runners: 2 }),
			// loaded but NO live runner advertises it → not available, must be hidden
			row({ model_id: 'mistral', available: false, state: 'loaded', serving_runners: 0 }),
			// still loading → not available
			row({ model_id: 'qwen', available: false, state: 'loading' }),
			// approved only → not available
			row({ model_id: 'phi', available: false, state: 'approved' })
		];
		expect(availableModelIds(models)).toEqual(['llama3']);
	});

	it('returns an empty list when nothing is available', () => {
		const models: ModelSetView[] = [
			row({ model_id: 'mistral', available: false, state: 'loaded', serving_runners: 0 }),
			row({ model_id: 'qwen', available: false, state: 'draining' })
		];
		expect(availableModelIds(models)).toEqual([]);
	});

	it('preserves order and includes available LoRA + base rows', () => {
		const models: ModelSetView[] = [
			row({ model_id: 'base-a', available: true, state: 'loaded', serving_runners: 1 }),
			row({
				model_id: 'lora-x',
				available: true,
				state: 'loaded',
				serving_runners: 1,
				base: 'base-a'
			}),
			row({ model_id: 'unserved', available: false, state: 'loaded' })
		];
		expect(availableModelIds(models)).toEqual(['base-a', 'lora-x']);
	});
});
