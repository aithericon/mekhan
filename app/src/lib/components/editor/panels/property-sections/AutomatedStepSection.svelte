<script lang="ts">
	import type { AutomatedStepNodeData, ExecutionBackendType } from '$lib/types/editor';
	import type { components } from '$lib/api/schema';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import { onMount } from 'svelte';
	import * as Select from '$lib/components/ui/select';
	import { Button } from '$lib/components/ui/button';
	import RotateCcw from '@lucide/svelte/icons/rotate-ccw';
	import PortsSection from './PortsSection.svelte';
	import { defaultOutputPort, emptyOutputPort } from '$lib/editor/automated-ports';
	import {
		backendList,
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

	function handleOutputPortChange(port: Port) {
		onchange({ ...data, output: port });
	}

	function resetOutputToBackendDefault() {
		onchange({ ...data, output: defaultOutputPort(data.executionSpec.backendType) });
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
				<Select.Item value="scheduled" label="Scheduled (Nomad/Slurm, GPU)" />
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
</div>

<div class="space-y-2 pt-3 border-t border-border/40">
	<div class="flex items-center justify-between">
		<span class="text-sm font-medium text-muted-foreground">Output port</span>
		{#if !readonly}
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
	<PortsSection
		port={outputPort}
		{readonly}
		title="Fields"
		emptyHint="No declared output fields. Downstream edges with declared input ports will type-mismatch on publish — click reset to seed the backend's default shape."
		onchange={handleOutputPortChange}
	/>
</div>
