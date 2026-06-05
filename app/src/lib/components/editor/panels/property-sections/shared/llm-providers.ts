/**
 * LLM provider → label / resource-kind maps, shared by the two authoring
 * surfaces (`LlmCommonFields` Agent + `LlmStepIdeEditor` AutomatedStep). Pure
 * data in a `.ts` module so it is one source of truth and unit-testable
 * without mounting a component.
 */

export const PROVIDER_LABELS: Record<string, string> = {
	openai: 'OpenAI',
	anthropic: 'Anthropic',
	ollama: 'Ollama',
	internal: 'Internal Model Pool'
};

/**
 * Per-provider resource type map. `openai` and `anthropic` each have a
 * workspace resource kind carrying their api_key + base_url; `ollama`
 * (usually keyless, local) still falls back to manual base_url entry until an
 * `ollama` resource kind ships. `internal` binds an `internal_llm` resource
 * pointing at the in-cluster pool router — its endpoint + credentials are
 * fixed by that resource and cannot be overridden per-step (GDPR).
 */
export const RESOURCE_TYPE_FOR_PROVIDER: Record<string, string | null> = {
	openai: 'openai',
	anthropic: 'anthropic',
	ollama: null,
	internal: 'internal_llm'
};
