<script lang="ts">
	import StepsView from '$lib/components/instances/StepsView.svelte';
	import { useInstanceContext } from '$lib/components/instances/instance-context';

	const ctx = useInstanceContext();
</script>

<div class="absolute inset-0 overflow-y-auto">
	{#if ctx.instance}
		<StepsView instance={ctx.instance} />
	{/if}
</div>
