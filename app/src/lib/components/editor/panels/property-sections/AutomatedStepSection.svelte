<script lang="ts">
	import type { AutomatedStepNodeData, ExecutionBackendType } from '$lib/types/editor';
	import type { components } from '$lib/api/schema';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import { onMount, untrack } from 'svelte';
	import * as Select from '$lib/components/ui/select';
	import { Button } from '$lib/components/ui/button';
	import { Checkbox } from '$lib/components/ui/checkbox';
	import RotateCcw from '@lucide/svelte/icons/rotate-ccw';
	import PortsSection from './PortsSection.svelte';
	import { defaultOutputPort, emptyOutputPort } from '$lib/editor/automated-ports';
	import {
		backendList,
		deriveBackendOutput,
		getCachedBackend,
		loadBackends
	} from '$lib/editor/backend-registry.svelte';
	import { BACKEND_PANELS } from '$lib/editor/backend-panels';
	import { GPU_JOB_TEMPLATES } from '$lib/editor/deployment-targets';
	import { FormField } from '$lib/components/ui/form-field';
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
	let deriveTimer: ReturnType<typeof setTimeout> | null = null;
	let deriveSeq = 0;
	$effect(() => {
		if (outputAuthoring !== 'derived' || readonly) return;
		const backendType = data.executionSpec.backendType;
		const cfg = data.executionSpec.config;
		if (deriveTimer) clearTimeout(deriveTimer);
		const seq = ++deriveSeq;
		deriveTimer = setTimeout(() => {
			deriveBackendOutput(backendType, cfg)
				.then((port) => {
					if (seq !== deriveSeq) return;
					untrack(() => {
						if (!portsEqual(data.output, port)) {
							onchange({ ...data, output: port });
						}
					});
				})
				.catch(() => {
					if (seq !== deriveSeq) return;
					untrack(() => {
						const fallback = defaultOutputPort(backendType);
						if (!portsEqual(data.output, fallback)) {
							onchange({ ...data, output: fallback });
						}
					});
				});
		}, 250);
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

	function portsEqual(a: Port | undefined, b: Port): boolean {
		if (!a) return false;
		if (a.id !== b.id || a.label !== b.label) return false;
		const af = a.fields ?? [];
		const bf = b.fields ?? [];
		if (af.length !== bf.length) return false;
		for (let i = 0; i < af.length; i++) {
			const x = af[i];
			const y = bf[i];
			if (
				x.name !== y.name ||
				x.kind !== y.kind ||
				x.label !== y.label ||
				(x.required ?? false) !== (y.required ?? false) ||
				(x.description ?? null) !== (y.description ?? null)
			) {
				return false;
			}
		}
		return true;
	}

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

	// Deployment model — inline (executor lifecycle) vs scheduled (scheduler-net,
	// Nomad/Slurm GPU). Optional-chained so legacy templates (no field) render
	// as inline; Rust `#[serde(default)]` covers the wire side.
	const deploymentMode = $derived(data.deploymentModel?.mode ?? 'inline');
	const jobTemplate = $derived(
		data.deploymentModel?.mode === 'scheduled' ? data.deploymentModel.jobTemplate : ''
	);
	// Hide the Scheduled toggle for backends the registry marks
	// non-schedulable (engine effects — catalogue_query today).
	const allowScheduled = $derived(currentBackend?.schedulable ?? true);

	function setDeploymentMode(mode: string) {
		onchange({
			...data,
			deploymentModel:
				mode === 'scheduled'
					? {
							mode: 'scheduled',
							jobTemplate: jobTemplate || GPU_JOB_TEMPLATES[0].value
						}
					: { mode: 'inline' }
		});
	}

	function setJobTemplate(v: string) {
		onchange({ ...data, deploymentModel: { mode: 'scheduled', jobTemplate: v } });
	}

	// Resource-pool claim. Presence (`resourcePool: {}`) = "requires a unit
	// from the shared GPU pool"; absence = no claim. v1 only checks presence,
	// so pool/units are reserved and not surfaced — this is a plain boolean.
	const requiresPool = $derived(data.resourcePool != null);

	// Mutual exclusion (compiler rejects pool + scheduled — the scheduler-net
	// owns its own admission): the pool toggle is disabled while scheduled,
	// and the Scheduled deployment option is disabled while the pool is on.
	const poolToggleDisabled = $derived(readonly || deploymentMode === 'scheduled');

	function setRequiresPool(on: boolean) {
		if (on) {
			// Empty object → v1 defaults (global resource-pool-net, weight 1).
			onchange({ ...data, resourcePool: {} });
		} else {
			// Destructure the key out so it's gone (not null) — Rust's
			// `skip_serializing_if = "Option::is_none"` only omits absent fields,
			// and the Yjs writer only persists the key when present.
			const { resourcePool: _omit, ...rest } = data;
			void _omit;
			onchange(rest as AutomatedStepNodeData);
		}
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

<div class="space-y-2 pt-3 border-t border-border/40">
	<span class="text-sm font-medium text-muted-foreground">Deployment</span>
	{#if allowScheduled}
		<Select.Root
			type="single"
			value={deploymentMode}
			onValueChange={(v) => {
				if (v) setDeploymentMode(v);
			}}
			disabled={readonly}
		>
			<Select.Trigger disabled={readonly} data-testid="select-deployment-model">
				{deploymentMode === 'scheduled'
					? 'Scheduled (Nomad/Slurm, GPU)'
					: 'Inline (immediate)'}
			</Select.Trigger>
			<Select.Content>
				<Select.Item value="inline" label="Inline (immediate)" />
				<Select.Item
					value="scheduled"
					label="Scheduled (Nomad/Slurm, GPU)"
					disabled={requiresPool}
				/>
			</Select.Content>
		</Select.Root>
		{#if deploymentMode === 'scheduled'}
			<FormField label="Job template" for="deployment-job-template">
				<Select.Root
					type="single"
					value={jobTemplate}
					onValueChange={(v) => {
						if (v) setJobTemplate(v);
					}}
					disabled={readonly}
				>
					<Select.Trigger disabled={readonly} data-testid="select-job-template">
						{GPU_JOB_TEMPLATES.find((t) => t.value === jobTemplate)?.label ??
							(jobTemplate || 'Select a job template…')}
					</Select.Trigger>
					<Select.Content>
						{#each GPU_JOB_TEMPLATES as t (t.value)}
							<Select.Item value={t.value} label={t.label} />
						{/each}
					</Select.Content>
				</Select.Root>
			</FormField>
			<p class="text-sm text-muted-foreground">
				Submitted through the scheduler-net, which owns queueing, GPU
				allocation and retry/backoff.
			</p>
		{/if}
	{:else}
		<p class="text-sm text-muted-foreground">
			Inline only — this backend runs as an engine effect.
		</p>
	{/if}

	<!-- Shared GPU pool claim. Presence-only in v1 (no pool/units knobs);
	     mutually exclusive with Scheduled (compiler rejects the combo — the
	     scheduler-net owns its own admission). -->
	<div class="space-y-1 pt-2">
		<label class="flex items-center gap-1.5 text-sm text-foreground">
			<Checkbox
				checked={requiresPool}
				disabled={poolToggleDisabled}
				onCheckedChange={(v) => setRequiresPool(v === true)}
				data-testid="toggle-resource-pool"
			/>
			Requires a shared GPU pool
		</label>
		{#if deploymentMode === 'scheduled'}
			<p class="text-sm text-muted-foreground">Not available for scheduled steps.</p>
		{:else}
			<p class="text-sm text-muted-foreground">
				Holds a unit from resource-pool-net for the step's duration; queues
				when the pool is full.
			</p>
		{/if}
	</div>
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
