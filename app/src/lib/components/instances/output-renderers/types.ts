/**
 * Output-rendering registry: each registered renderer declares a `matches`
 * predicate over a runtime value + light context, and a Svelte component that
 * knows how to render that shape nicely. `SmartValue` picks the first matching
 * renderer; `JsonBlock` is the always-on fallback.
 *
 * Shapes targeted are real envelopes the engine parks at `data_port` —
 * HumanTask response envelopes (`{task_id, status, data: {form}, ...}`),
 * catalogue file references (`{url, filename, content_type}`), declared
 * producer outputs, etc. See `service/src/compiler/token_shape.rs` for the
 * canonical shape documentation.
 */
import type { Component } from 'svelte';

export type RenderPosition = 'output' | 'input';

export type RenderContext = {
	/** The producing node's kind (snake_case, e.g. "human_task"). Lets shape-
	 *  ambiguous renderers (HumanTaskEnvelope) confirm. Optional — renderers
	 *  must still work on shape alone. */
	nodeKind?: string;
	/** Where this value is being shown — slight visual tweaks (e.g. inputs get
	 *  the producer name above; outputs don't). */
	position: RenderPosition;
	/** Owning workflow instance id (UUID). Renderers that need to reach back
	 *  out to instance-scoped backend resources (e.g. AutomatedStepEnvelope
	 *  fetching live log lines for an execution_id) read this; renderers
	 *  without that need ignore it. */
	instanceId?: string;
	/** Step's ISO start time. AutomatedStepEnvelope uses this as the lower
	 *  bound when fetching logs for this execution — robust to backends that
	 *  don't stamp `execution_id` on every log line (e.g. the in-process LLM
	 *  backend, whose tracing logs route through StreamContext without the
	 *  IPC sidecar's field auto-stamping). */
	stepStartedAt?: string;
	/** Step's ISO completion time. Upper bound for the same log fetch.
	 *  When the step is still running, the renderer falls back to "now". */
	stepCompletedAt?: string;
	/** When the surrounding view already renders a dedicated, step-scoped logs
	 *  section (e.g. the instance side panel's first-class "Logs" section), the
	 *  AutomatedStepEnvelope suppresses its own inline logs block to avoid
	 *  showing the same lines twice. Defaults to false (envelopes shown in
	 *  process views / inputs keep their inline logs). */
	suppressLogs?: boolean;
	/** Hide the collapsed "Process metadata" (`_*` keys) disclosure. Set by
	 *  views that already display the run's identity around the envelope (the
	 *  instance overview's Inputs/Results cards sit under a header carrying
	 *  instance id / template / timestamps), where the plumbing keys are pure
	 *  noise. Defaults to false — standalone surfaces (task pages, step
	 *  drawers) keep the disclosure. */
	hideProcessMetadata?: boolean;
};

export type RendererProps = {
	value: unknown;
	ctx: RenderContext;
};

export type OutputRenderer = {
	/** Stable id used as the toggle key when the user flips to raw JSON. */
	name: string;
	/** Human-readable label shown in the "rendered as …" hint. */
	label: string;
	matches: (value: unknown, ctx: RenderContext) => boolean;
	component: Component<RendererProps>;
};
