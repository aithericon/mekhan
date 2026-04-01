<script lang="ts">
	import type { AutomatedStepNodeData, ExecutionBackendType } from '$lib/types/editor';
	import * as Select from '$lib/components/ui/select';
	import PythonConfigPanel from './automated/PythonConfigPanel.svelte';
	import DockerConfigPanel from './automated/DockerConfigPanel.svelte';
	import ProcessConfigPanel from './automated/ProcessConfigPanel.svelte';
	import HttpConfigPanel from './automated/HttpConfigPanel.svelte';
	import LlmConfigPanel from './automated/LlmConfigPanel.svelte';
	import FileOpsConfigPanel from './automated/FileOpsConfigPanel.svelte';
	import KreuzbergConfigPanel from './automated/KreuzbergConfigPanel.svelte';
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';

	type Props = {
		data: AutomatedStepNodeData;
		readonly?: boolean;
		onchange: (data: AutomatedStepNodeData) => void;
		onexpand?: () => void;
		binding?: YjsGraphBinding;
		nodeId?: string;
	};

	let { data, readonly = false, onchange, onexpand, binding, nodeId }: Props = $props();

	const defaultConfigs: Record<ExecutionBackendType, Record<string, unknown>> = {
		python: { script: '', timeout_seconds: 30 },
		docker: { image: '', env: {} },
		process: { command: '', args: [] },
		http: { method: 'GET', url: '' },
		llm: { provider: 'openai', model: '', prompt: '' },
		file_ops: { operation: 'stat', path: '', storage: { backend: 'local', endpoint: '' } },
		kreuzberg: { mode: 'single' }
	};

	const backendLabels: Record<ExecutionBackendType, string> = {
		python: 'Python',
		process: 'Process',
		docker: 'Docker',
		http: 'HTTP Request',
		llm: 'LLM (AI Model)',
		file_ops: 'File Operations',
		kreuzberg: 'Document Extraction'
	};

	function handleBackendTypeChange(backendType: ExecutionBackendType) {
		onchange({
			...data,
			executionSpec: {
				backendType,
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
</script>

<div class="space-y-1.5">
	<span class="text-xs font-medium text-muted-foreground">Backend Type</span>
	<Select.Root
		type="single"
		value={data.executionSpec.backendType}
		onValueChange={(v) => { if (v) handleBackendTypeChange(v as ExecutionBackendType); }}
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
		</Select.Content>
	</Select.Root>
</div>

{#if data.executionSpec.backendType === 'python'}
	<PythonConfigPanel config={data.executionSpec.config} {readonly} onchange={handleConfigChange} {onexpand} {binding} {nodeId} />
{:else if data.executionSpec.backendType === 'docker'}
	<DockerConfigPanel config={data.executionSpec.config} {readonly} onchange={handleConfigChange} />
{:else if data.executionSpec.backendType === 'process'}
	<ProcessConfigPanel config={data.executionSpec.config} {readonly} onchange={handleConfigChange} />
{:else if data.executionSpec.backendType === 'http'}
	<HttpConfigPanel config={data.executionSpec.config} {readonly} onchange={handleConfigChange} />
{:else if data.executionSpec.backendType === 'llm'}
	<LlmConfigPanel config={data.executionSpec.config} {readonly} onchange={handleConfigChange} />
{:else if data.executionSpec.backendType === 'file_ops'}
	<FileOpsConfigPanel config={data.executionSpec.config} {readonly} onchange={handleConfigChange} />
{:else if data.executionSpec.backendType === 'kreuzberg'}
	<KreuzbergConfigPanel config={data.executionSpec.config} {readonly} onchange={handleConfigChange} />
{/if}
