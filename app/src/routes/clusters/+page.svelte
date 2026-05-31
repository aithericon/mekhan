<script lang="ts">
	import ClusterList from '$lib/components/clusters/ClusterList.svelte';
</script>

<svelte:head><title>Clusters | Mekhan</title></svelte:head>

<div class="h-full overflow-y-auto" data-testid="clusters-page">
	<div class="mx-auto max-w-6xl px-6 py-8 animate-rise">
		<ClusterList />
	</div>
</div>
