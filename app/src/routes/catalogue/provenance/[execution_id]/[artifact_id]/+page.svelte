<script lang="ts">
	import { page } from '$app/stores';
	import {
		getProvenanceFromArtifact,
		getCatalogueEntry,
		type AncestryNode,
		type CrossNetEdge,
		type CatalogueEntry
	} from '$lib/api/client';
	import { instanceIdFromNet, instanceIdFromExecution } from '$lib/utils';
	import { ProvenanceCanvas } from '$lib/components/provenance';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import ArrowLeft from '@lucide/svelte/icons/arrow-left';
	import GitBranch from '@lucide/svelte/icons/git-branch';
	import Activity from '@lucide/svelte/icons/activity';

	let ancestry = $state<AncestryNode[]>([]);
	let crossNetEdges = $state<CrossNetEdge[]>([]);
	let artifact = $state<CatalogueEntry | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);

	const executionId = $derived($page.params.execution_id);
	const artifactId = $derived($page.params.artifact_id);
	// Producing instance: source_net is the net_id (`mekhan-{ws}-{instance}`);
	// execution_id is `mekhan-{ws}-{inst}-{run}`. Either resolves to /instances/{id}.
	const instanceId = $derived(
		instanceIdFromNet(artifact?.source_net) ?? instanceIdFromExecution(executionId)
	);

	$effect(() => {
		if (executionId && artifactId) {
			loadProvenance(executionId, artifactId);
		}
	});

	async function loadProvenance(execId: string, artId: string) {
		loading = true;
		error = null;
		try {
			const resp = await getProvenanceFromArtifact(execId, artId, 30);
			ancestry = resp.nodes;
			crossNetEdges = resp.cross_net_edges;

			// Also load the artifact metadata for the header (non-critical;
			// the page degrades gracefully if it fails).
			try {
				artifact = await getCatalogueEntry(execId, artId);
			} catch {
				// Header just won't show artifact details.
			}
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load provenance';
		} finally {
			loading = false;
		}
	}
</script>

<svelte:head>
	<title>{artifact?.name ?? 'Provenance'} | Mekhan</title>
</svelte:head>

<div class="flex h-screen flex-col">
	<!-- Header -->
	<div class="flex items-center gap-3 border-b px-4 py-3">
		<Button variant="ghost" size="icon" href="/data">
			<ArrowLeft class="h-4 w-4" />
		</Button>

		<GitBranch class="h-5 w-5 text-zinc-400" />

		<div class="min-w-0 flex-1">
			<h1 class="text-lg font-semibold truncate">
				{#if artifact}
					{artifact.name}
				{:else}
					Provenance
				{/if}
			</h1>
			{#if artifact}
				<div class="flex items-center gap-2 text-sm text-zinc-500">
					<Badge variant="outline">{artifact.category}</Badge>
					<span>{artifact.filename}</span>
					{#if artifact.source_net}
						<span>&middot; {artifact.source_net}</span>
					{/if}
				</div>
			{/if}
		</div>

		{#if ancestry.length > 0}
			<Badge variant="secondary">{ancestry.length} events</Badge>
		{/if}

		{#if instanceId}
			<Button variant="outline" size="sm" href="/instances/{instanceId}/process" class="gap-1.5">
				<Activity class="h-4 w-4" />
				Open instance
			</Button>
		{/if}
	</div>

	<!-- Canvas -->
	<div class="flex-1">
		{#if loading}
			<div class="flex h-full items-center justify-center text-zinc-400">
				Loading provenance chain...
			</div>
		{:else if error}
			<div class="flex h-full items-center justify-center">
				<div class="rounded-md bg-red-50 p-4 text-sm text-red-700 dark:bg-red-900/20 dark:text-red-300">
					{error}
				</div>
			</div>
		{:else}
			<ProvenanceCanvas {ancestry} {crossNetEdges} />
		{/if}
	</div>
</div>
