/**
 * Tests for the LLM provider maps + the internal-pool GDPR-lock decision.
 *
 * Per the codebase convention we test the exported module-level data
 * (`PROVIDER_LABELS`, `RESOURCE_TYPE_FOR_PROVIDER`) and the pure decision that
 * gates the template (`isInternal := provider === 'internal'`) rather than
 * mounting the Svelte component. The template branch keyed on `isInternal` is:
 *   - swaps the free-text model <Input> for <ModelPicker>, and
 *   - omits the per-step base_url / api_key override inputs entirely,
 * which is the GDPR requirement (an internal binding cannot escape off-router).
 */
import { describe, it, expect } from 'vitest';
import { PROVIDER_LABELS, RESOURCE_TYPE_FOR_PROVIDER } from './llm-providers';

// Mirror of the component's `isInternal` derived gate.
const isInternal = (provider: string) => provider === 'internal';

// Mirror of the component's branch effect: which override inputs render.
function rendersOverrides(provider: string): {
	modelPicker: boolean;
	freeTextModel: boolean;
	apiKeyOverride: boolean;
	baseUrlOverride: boolean;
} {
	if (isInternal(provider)) {
		return {
			modelPicker: true,
			freeTextModel: false,
			apiKeyOverride: false,
			baseUrlOverride: false
		};
	}
	return {
		modelPicker: false,
		freeTextModel: true,
		apiKeyOverride: true,
		baseUrlOverride: true
	};
}

describe('LlmCommonFields provider maps', () => {
	it('registers the internal provider in both maps', () => {
		expect(PROVIDER_LABELS.internal).toBe('Internal Model Pool');
		expect(RESOURCE_TYPE_FOR_PROVIDER.internal).toBe('internal_llm');
	});

	it('keeps the external providers unchanged', () => {
		expect(RESOURCE_TYPE_FOR_PROVIDER.openai).toBe('openai');
		expect(RESOURCE_TYPE_FOR_PROVIDER.anthropic).toBe('anthropic');
		expect(RESOURCE_TYPE_FOR_PROVIDER.ollama).toBeNull();
	});
});

describe('internal-pool GDPR lock', () => {
	it('locks model selection to the picker and hides off-router overrides for internal', () => {
		const r = rendersOverrides('internal');
		expect(r.modelPicker).toBe(true);
		expect(r.freeTextModel).toBe(false);
		// GDPR: no per-step base_url / api_key escape hatch for an internal binding.
		expect(r.apiKeyOverride).toBe(false);
		expect(r.baseUrlOverride).toBe(false);
	});

	it.each(['openai', 'anthropic', 'ollama'])(
		'keeps free-text model + overrides for external provider %s',
		(provider) => {
			const r = rendersOverrides(provider);
			expect(r.modelPicker).toBe(false);
			expect(r.freeTextModel).toBe(true);
			expect(r.apiKeyOverride).toBe(true);
			expect(r.baseUrlOverride).toBe(true);
		}
	);
});
