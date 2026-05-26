<script lang="ts">
	import type { AutomatedStepNodeData, ExecutionBackendType } from '$lib/types/editor';
	import type { components } from '$lib/api/schema';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import * as Select from '$lib/components/ui/select';
	import { Button } from '$lib/components/ui/button';
	import RotateCcw from '@lucide/svelte/icons/rotate-ccw';
	import PythonConfigPanel from './automated/PythonConfigPanel.svelte';
	import DockerConfigPanel from './automated/DockerConfigPanel.svelte';
	import ProcessConfigPanel from './automated/ProcessConfigPanel.svelte';
	import HttpConfigPanel from './automated/HttpConfigPanel.svelte';
	import LlmConfigPanel from './automated/LlmConfigPanel.svelte';
	import FileOpsConfigPanel from './automated/FileOpsConfigPanel.svelte';
	import KreuzbergConfigPanel from './automated/KreuzbergConfigPanel.svelte';
	import SmtpConfigPanel from './automated/SmtpConfigPanel.svelte';
	import CatalogueQueryConfigPanel from './automated/CatalogueQueryConfigPanel.svelte';
	import PortsSection from './PortsSection.svelte';
	import { defaultOutputPort, emptyOutputPort } from '$lib/editor/automated-ports';
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

	const outputPort = $derived<Port>(data.output ?? emptyOutputPort());

	function handleOutputPortChange(port: Port) {
		onchange({ ...data, output: port });
	}

	function resetOutputToBackendDefault() {
		onchange({ ...data, output: defaultOutputPort(data.executionSpec.backendType) });
	}

	const defaultConfigs: Record<ExecutionBackendType, Record<string, unknown>> = {
		python: { python: 'python3', requirements: [], virtualenv: false, sdk: true, inherit_env: true, env: {} },
		docker: { image: '', env: {} },
		process: { command: '', args: [] },
		http: { method: 'GET', url: '' },
		llm: { provider: 'openai', model: '', prompt: '' },
		file_ops: { operation: 'stat', path: '', storage: { backend: 'local', endpoint: '' } },
		kreuzberg: { mode: 'single' },
		smtp: {
			resource_alias: '',
			to: [],
			cc: [],
			bcc: [],
			subject: { label: 'subject.tera', source: 'Hello {{ intake.name }}' },
			body_text: { label: 'body.txt.tera', source: 'Hi {{ intake.name }},\n\nThanks!\n' },
			attachments: [],
			dry_run: false,
			vars: {}
		},
		catalogue_query: { category: '', limit: 50 }
	};

	const backendLabels: Record<ExecutionBackendType, string> = {
		python: 'Python',
		process: 'Process',
		docker: 'Docker',
		http: 'HTTP Request',
		llm: 'LLM (AI Model)',
		file_ops: 'File Operations',
		kreuzberg: 'Document Extraction',
		smtp: 'SMTP (Email)',
		catalogue_query: 'Catalogue Query'
	};

	function isExecutionBackendType(v: string): v is ExecutionBackendType {
		return v in backendLabels;
	}

	function handleBackendTypeChange(backendType: ExecutionBackendType) {
		onchange({
			...data,
			executionSpec: {
				backendType,
				entrypoint: backendType === 'python' ? (data.executionSpec.entrypoint ?? 'main.py') : data.executionSpec.entrypoint,
				config: defaultConfigs[backendType]
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

	// Deployment model — inline (executor lifecycle) vs scheduled (scheduler-net,
	// Nomad/Slurm GPU). Optional-chained so legacy templates (no field) render
	// as inline; Rust `#[serde(default)]` covers the wire side.
	const deploymentMode = $derived(data.deploymentModel?.mode ?? 'inline');
	const jobTemplate = $derived(
		data.deploymentModel?.mode === 'scheduled' ? data.deploymentModel.jobTemplate : ''
	);

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
		onValueChange={(v) => { if (v && isExecutionBackendType(v)) handleBackendTypeChange(v); }}
		disabled={readonly}
	>
		<Select.Trigger disabled={readonly}>
			{backendLabels[data.executionSpec.backendType] ?? data.executionSpec.backendType}
		</Select.Trigger>
		<Select.Content>
			<Select.Item value="python" label="Python" />
			<Select.Item value="process" label="Process" />
			<Select.Item value="docker" label="Docker" />
			<Select.Item value="http" label="HTTP Request" />
			<Select.Item value="llm" label="LLM (AI Model)" />
			<Select.Item value="file_ops" label="File Operations" />
			<Select.Item value="kreuzberg" label="Document Extraction" />
			<Select.Item value="smtp" label="SMTP (Email)" />
			<Select.Item value="catalogue_query" label="Catalogue Query" />
		</Select.Content>
	</Select.Root>
</div>

{#if data.executionSpec.backendType === 'python'}
	<PythonConfigPanel
		config={data.executionSpec.config as Record<string, unknown>}
		entrypoint={data.executionSpec.entrypoint ?? 'main.py'}
		{readonly}
		onchange={handleConfigChange}
		onentrypointchange={handleEntrypointChange}
		{binding}
		{nodeId}
		{templateId}
	/>
{:else if data.executionSpec.backendType === 'docker'}
	<DockerConfigPanel config={data.executionSpec.config as Record<string, unknown>} {readonly} onchange={handleConfigChange} />
{:else if data.executionSpec.backendType === 'process'}
	<ProcessConfigPanel config={data.executionSpec.config as Record<string, unknown>} {readonly} onchange={handleConfigChange} />
{:else if data.executionSpec.backendType === 'http'}
	<HttpConfigPanel
		config={data.executionSpec.config as Record<string, unknown>}
		{readonly}
		onchange={handleConfigChange}
		{binding}
		{nodeId}
		{templateId}
		{scope}
	/>
{:else if data.executionSpec.backendType === 'llm'}
	<LlmConfigPanel config={data.executionSpec.config as Record<string, unknown>} {readonly} onchange={handleConfigChange} {scope} />
{:else if data.executionSpec.backendType === 'file_ops'}
	<FileOpsConfigPanel config={data.executionSpec.config as Record<string, unknown>} {readonly} onchange={handleConfigChange} />
{:else if data.executionSpec.backendType === 'kreuzberg'}
	<KreuzbergConfigPanel config={data.executionSpec.config as Record<string, unknown>} {readonly} onchange={handleConfigChange} {scope} />
{:else if data.executionSpec.backendType === 'smtp'}
	<SmtpConfigPanel config={data.executionSpec.config as Record<string, unknown>} {readonly} onchange={handleConfigChange} {scope} />
{:else if data.executionSpec.backendType === 'catalogue_query'}
	<CatalogueQueryConfigPanel config={data.executionSpec.config as Record<string, unknown>} {readonly} onchange={handleConfigChange} />
{/if}

<div class="space-y-2 pt-3 border-t border-border/40">
	<span class="text-sm font-medium text-muted-foreground">Deployment</span>
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
