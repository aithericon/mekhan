<script lang="ts">
	import { NetWorkbench } from '$lib/components/petri';
	import { useInstanceContext } from '$lib/components/instances/instance-context';

	const ctx = useInstanceContext();
</script>

<div class="absolute inset-0">
	{#if ctx.instance?.net_id}
		<NetWorkbench netId={ctx.instance.net_id} />
	{/if}
</div>
