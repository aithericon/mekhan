<script lang="ts">
	import type { AutomatedStepNodeData } from '$lib/types/editor';
	import BackendConfigPanel from './BackendConfigPanel.svelte';
	import DeploymentSection from './DeploymentSection.svelte';
	import { resolveBackendMeta } from '$lib/editor/backend-panels';

	let { data = $bindable() }: { data: AutomatedStepNodeData } = $props();

	let backendType = $derived(data.executionSpec?.backendType ?? 'python');
	let backendMeta = $derived(resolveBackendMeta(backendType));

	// Whether this backend supports the executor deployment model (pool/scheduled).
	// Engine-effect backends (e.g. catalogue_query) don't.
	let supportsDeployment = $derived(backendMeta?.dispatchMode !== 'engine_effect');

	// Streaming side-channel (prototype): expose a `stream` output port that
	// fires once per `set_output(...)` the job emits mid-execution. Bound to the
	// node's `streamOutput` flag; the compiler mints a Signal `p_{id}_stream`
	// place + registers the "stream" handle when set.
	let streamOutput = $derived(data.streamOutput ?? false);

	function toggleStreamOutput(e: Event) {
		const checked = (e.target as HTMLInputElement).checked;
		data.streamOutput = checked;
	}
</script>

<div class="space-y-4">
	<BackendConfigPanel bind:data />

	{#if supportsDeployment}
		<DeploymentSection bind:data />
	{/if}

	<!--
		Streaming output (prototype). A checkbox that opts this step into the
		mid-execution `stream` port. Kept minimal — no per-event config yet.
	-->
	<label class="flex items-center gap-2 text-sm">
		<input type="checkbox" checked={streamOutput} onchange={toggleStreamOutput} />
		<span>Stream output (prototype)</span>
	</label>
</div>

<style>
</style>
