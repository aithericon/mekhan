<script lang="ts">
	import type { AutomatedStepNodeData, ExecutionBackendType } from '$lib/types/editor';
	import type { components } from '$lib/api/schema';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import { onMount, untrack } from 'svelte';
	import { portsEqual } from '$lib/editor/port-utils';
	import { createDebouncedFetcher } from '$lib/editor/debounced-fetcher';
	import * as Select from '$lib/components/ui/select';
	import { Button } from '$lib/components/ui/button';
	import RotateCcw from '@lucide/svelte/icons/rotate-ccw';
	import PortsSection from './PortsSection.svelte';
	import DeploymentSection from './DeploymentSection.svelte';
	import { defaultOutputPort, emptyOutputPort } from '$lib/editor/automated-ports';
	import {
		backendList,
		deriveBackendOutput,
		getCachedBackend,
		loadBackends
	} from '$lib/editor/backend-registry.svelte';
	import { BACKEND_PANELS } from '$lib/editor/backend-panels';
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';

	type Port = components['schemas']['Port'];

	type Props = {
		data: AutomatedStepNodeData;
		readonly?: boolean;
		onchange: (data: AutomatedStepNodeData) => void;
		binding?: YjsGraphBinding;
		nodeId?: string;
		templateId?: string;
		scope?: ScopeEntry[];
	};

	let {
		data,
		readonly = false,
		onchange,
		binding,
		nodeId,
		templateId,
		scope = []
	}: Props = $props();

	// Registry is loaded once in +layout.svelte; this is a safety net for
	// direct deep-links to editor routes that mount before the layout
	// finishes its onMount.
	onMount(() => {
		loadBackends().catch(() => {
			/* descriptors render empty; user sees a Loading… placeholder */
		});
	});

	const outputPort = $derived<Port>(data.output ?? emptyOutputPort());
	const backends = $derived(backendList());
	const currentBackend = $derived(getCachedBackend(data.executionSpec.backendType));
	const CurrentPanel = $derived(BACKEND_PANELS[data.executionSpec.backendType]);
	// `free` → user owns the port (current behavior, with Reset button).
	// `fixed` → backend's canonical default; read-only.
	// `derived` → server-derived from config; read-only and refetched on change.
	// Defaults to `free` so callers that pre-date the field (or hit the
	// registry before it loads) keep the legacy editable surface.
	const outputAuthoring = $derived(currentBackend?.outputAuthoring ?? 'free');

	function handleOutputPortChange(port: Port) {
		onchange({ ...data, output: port });
	}

	function resetOutputToBackendDefault() {
		onchange({ ...data, output: defaultOutputPort(data.executionSpec.backendType) });
	}

	// Derived-authoring effect: whenever the backend is `derived` and the
	// step config changes, fetch the canonical port shape from the server
	// and persist it onto `data.output`. The compiler is the source of
	// truth, so the editor never derives locally — drift is impossible.
	//
	// Debounced lightly (250ms) so rapid keystrokes in the JSON-schema
	// editor don't flood the endpoint. On failure we fall back to the
	// backend's default port shape from the descriptor rather than
	// silently keeping a stale `data.output`.
	const deriveFetcher = createDebouncedFetcher();
	$effect(() => {
		if (outputAuthoring !== 'derived' || readonly) return;
		const backendType = data.executionSpec.backendType;
		const cfg = data.executionSpec.config;
		deriveFetcher.schedule(async (fresh) => {
			try {
				const port = await deriveBackendOutput(backendType, cfg);
				if (!fresh()) return;
				untrack(() => {
					if (!portsEqual(data.output, port)) {
						onchange({ ...data, output: port });
					}
				});
			} catch {
				if (!fresh()) return;
				untrack(() => {
					const fallback = defaultOutputPort(backendType);
					if (!portsEqual(data.output, fallback)) {
						onchange({ ...data, output: fallback });
					}
				});
			}
		});
	});

	// Fixed-authoring effect: backend owns the canonical shape, so
	// overwrite any legacy/customized `data.output` with the descriptor's
	// default on first paint and whenever the backend changes. No debounce
	// — the descriptor is static.
	$effect(() => {
		if (outputAuthoring !== 'fixed' || readonly) return;
		const fixed = defaultOutputPort(data.executionSpec.backendType);
		untrack(() => {
			if (!portsEqual(data.output, fixed)) {
				onchange({ ...data, output: fixed });
			}
		});
	});

	function handleBackendTypeChange(backendType: ExecutionBackendType) {
		const decl = getCachedBackend(backendType);
		// Registry-supplied seed config. If the descriptor hasn't loaded
		// yet (rare — see onMount), fall back to an empty object; the user
		// can fill in fields manually until the panel renders.
		const defaultConfig =
			(decl?.defaultEditorConfig as Record<string, unknown> | undefined) ?? {};
		onchange({
			...data,
			executionSpec: {
				backendType,
				entrypoint:
					backendType === 'python'
						? (data.executionSpec.entrypoint ?? 'main.py')
						: data.executionSpec.entrypoint,
				config: defaultConfig
			}
		});
	}

	function handleConfigChange(config: Record<string, unknown>) {
		onchange({
			...data,
			executionSpec: { ...data.executionSpec, config }
		});
	}

	function handleEntrypointChange(entrypoint: string) {
		onchange({
			...data,
			executionSpec: { ...data.executionSpec, entrypoint }
		});
	}

	function isExecutionBackendType(v: string): v is ExecutionBackendType {
		return backends.some((b) => b.name === v);
	}

	// Hide the Scheduled toggle for backends the registry marks
	// non-schedulable (engine effects — catalogue_query today). The deployment
	// editor itself lives in the shared `DeploymentSection` component (also used
	// by the Agent panel).
	const allowScheduled = $derived(currentBackend?.schedulable ?? true);

	// Streaming side-channel (prototype): expose a `stream` output port that
	// fires once per `set_output(...)` the job emits mid-execution. Bound to
	// the node's `streamOutput` flag; the compiler mints a Signal `p_{id}_stream`
	// place + registers the "stream" handle when set.
	const streamOutput = $derived(data.streamOutput ?? false);

	function toggleStreamOutput(e: Event) {
		const checked = (e.target as HTMLInputElement).checked;
		onchange({ ...data, streamOutput: checked });
	}

	// Streaming input (reducer): make this step a long-lived in-process reducer
	// fed the upstream producer's chunks over IPC (`aithericon.chunks()`). Bound
	// to the node's `streamInput` flag; the compiler seeds the job at net entry,
	// exposes a "stream" INPUT handle, and routes the control `in` edge as EOF.
	const streamInput = $derived(data.streamInput ?? false);

	function toggleStreamInput(e: Event) {
		const checked = (e.target as HTMLInputElement).checked;
		onchange({ ...data, streamInput: checked });
	}
</script>

<div class="space-y-1.5">
	<span class="text-sm font-medium text-muted-foreground">Backend Type</span>
	<Select.Root
		type="single"
		value={data.executionSpec.backendType}
		onValueChange={(v) => {
			if (v && isExecutionBackendType(v)) handleBackendTypeChange(v);
		}}
		disabled={readonly}
	>
		<Select.Trigger disabled={readonly}>
			{currentBackend?.displayName ?? data.executionSpec.backendType}
		</Select.Trigger>
		<Select.Content>
			{#each backends as b (b.name)}
				<Select.Item value={b.name} label={b.displayName} />
			{/each}
		</Select.Content>
	</Select.Root>
</div>

{#if CurrentPanel}
	<CurrentPanel
		config={data.executionSpec.config as Record<string, unknown>}
		entrypoint={data.executionSpec.entrypoint ?? 'main.py'}
		{readonly}
		onchange={handleConfigChange}
		onentrypointchange={handleEntrypointChange}
		{binding}
		{nodeId}
		{templateId}
		{scope}
	/>
{/if}

<DeploymentSection
	value={data.deploymentModel}
	schedulable={allowScheduled}
	requirements={data.requirements}
	onRequirementsChange={(requirements) => onchange({ ...data, requirements })}
	{readonly}
	onchange={(dm) => onchange({ ...data, deploymentModel: dm })}
/>

<!--
	Streaming output (prototype). A checkbox that opts this step into the
	mid-execution `stream` port. Kept minimal — no per-event config yet.
-->
<div class="space-y-1 pt-3 border-t border-border/40">
	<label class="flex items-center gap-2 text-sm">
		<input
			type="checkbox"
			checked={streamOutput}
			disabled={readonly}
			onchange={toggleStreamOutput}
		/>
		<span>Stream output (prototype)</span>
	</label>
	<p class="text-sm text-muted-foreground">
		Emits a <code class="font-mono">stream</code> handle that fires once per
		<code class="font-mono">set_output(…)</code> call during execution.
	</p>
</div>

<!--
	Streaming input (reducer). Opts this step into being a long-lived stateful
	reducer fed the producer's chunks over IPC. Wire producer.stream →
	this.stream and producer.out → this.in.
-->
<div class="space-y-1 pt-3 border-t border-border/40">
	<label class="flex items-center gap-2 text-sm">
		<input
			type="checkbox"
			checked={streamInput}
			disabled={readonly}
			onchange={toggleStreamInput}
		/>
		<span>Stream input (reducer)</span>
	</label>
	<p class="text-sm text-muted-foreground">
		Exposes a <code class="font-mono">stream</code> input handle; the step is seeded
		at net entry and reads chunks via <code class="font-mono">aithericon.chunks()</code>.
		Wire the producer's <code class="font-mono">stream</code> handle here and its
		<code class="font-mono">out</code> to this node's <code class="font-mono">in</code> (the EOF trigger).
	</p>
</div>

<div class="space-y-2 pt-3 border-t border-border/40">
	<div class="flex items-center justify-between">
		<span class="text-sm font-medium text-muted-foreground">Output port</span>
		{#if !readonly && outputAuthoring === 'free'}
			<Button
				variant="ghost"
				size="sm"
				onclick={resetOutputToBackendDefault}
				class="h-7 gap-1 px-2 text-sm"
				title="Reset output port to the backend's canonical shape"
			>
				<RotateCcw class="size-3.5" />
				Reset to {data.executionSpec.backendType} default
			</Button>
		{/if}
	</div>
	{#if outputAuthoring === 'derived'}
		<p class="text-sm text-muted-foreground">
			Derived from this step's config — edit the config above (response format, schema) to change the
			output fields. The runner produces this exact shape; declaring it manually would only drift.
		</p>
	{:else if outputAuthoring === 'fixed'}
		<p class="text-sm text-muted-foreground">
			Fixed canonical shape for this backend. The runner always emits these fields.
		</p>
	{/if}
	<PortsSection
		port={outputPort}
		readonly={readonly || outputAuthoring !== 'free'}
		title="Fields"
		emptyHint={outputAuthoring === 'derived'
			? 'No fields yet — pick a response format on this step to define them.'
			: "No declared output fields. Downstream edges with declared input ports will type-mismatch on publish — click reset to seed the backend's default shape."}
		onchange={handleOutputPortChange}
	/>
</div>
