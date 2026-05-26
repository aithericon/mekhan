<script lang="ts">
	import WorkflowGraphView from '$lib/components/instances/WorkflowGraphView.svelte';
	import { useInstanceContext } from '$lib/components/instances/instance-context';

	const ctx = useInstanceContext();
</script>

<div class="absolute inset-0">
	{#if ctx.instance}
		<WorkflowGraphView instance={ctx.instance} />
	{/if}
</div>
