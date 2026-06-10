<script lang="ts">
	import { page } from '$app/stores';
	import { goto } from '$app/navigation';
	import { PageShell } from '$lib/components/shell';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import { NetWorkbench } from '$lib/components/petri';
	import type { WorkbenchApi } from '$lib/components/petri/NetWorkbench.svelte';
	import ArrowLeft from '@lucide/svelte/icons/arrow-left';
	import Play from '@lucide/svelte/icons/play';
	import Pause from '@lucide/svelte/icons/pause';
	import RotateCcw from '@lucide/svelte/icons/rotate-ccw';
	import RefreshCw from '@lucide/svelte/icons/refresh-cw';
	import FileText from '@lucide/svelte/icons/file-text';
	import PanelLeftClose from '@lucide/svelte/icons/panel-left-close';
	import PanelLeftOpen from '@lucide/svelte/icons/panel-left-open';

	const PETRI_URL = '/petri';
	const netId = $derived($page.params.id as string);
	// Engine nets launched from a mekhan instance are keyed "mekhan-{instanceId}".
	const owningInstanceId = $derived(
		netId.startsWith('mekhan-') ? netId.slice('mekhan-'.length) : null
	);

	async function handleDeleteNet(id: string) {
		if (!confirm(`Delete net "${id}"?`)) return;
		try {
			await fetch(`${PETRI_URL}/api/nets/${id}`, { method: 'DELETE' });
			if (id === netId) goto('/nets');
		} catch {
			/* ignore */
		}
	}
</script>

<svelte:head>
	<title>{netId} | Engine Nets | Mekhan</title>
</svelte:head>

{#snippet header(api: WorkbenchApi)}
	<div class="flex items-center gap-3 border-b border-border px-4 py-2 shrink-0">
		<Button variant="ghost" size="icon-sm" href="/nets">
			<ArrowLeft class="size-4" />
		</Button>
		<Button variant="ghost" size="icon-sm" onclick={api.toggleNetTree}>
			{#if api.netTreeOpen}
				<PanelLeftClose class="size-4" />
			{:else}
				<PanelLeftOpen class="size-4" />
			{/if}
		</Button>
		<div class="flex items-center gap-2">
			<span class="font-mono text-sm font-medium">{netId}</span>
			{#if api.store.runMode}
				<Badge
					class={api.store.runMode === 'running'
						? 'bg-blue-100 text-blue-700 dark:bg-blue-900/30 dark:text-blue-400'
						: 'bg-gray-100 text-gray-700 dark:bg-gray-800 dark:text-gray-400'}
				>
					{api.store.runMode}
				</Badge>
			{/if}
			{#if owningInstanceId}
				<Button variant="ghost" size="sm" href="/instances/{owningInstanceId}">
					Instance ▸
				</Button>
			{/if}
		</div>
		<div class="ml-auto flex items-center gap-1">
			<Button variant="outline" size="sm" onclick={api.openScenario}>
				<FileText class="size-3.5" /> Scenario
			</Button>
			<Button variant="outline" size="sm" onclick={() => api.store.reset()}>
				<RotateCcw class="size-3.5" /> Reset
			</Button>
			<Button
				variant="outline"
				size="sm"
				onclick={() =>
					api.store.setRunMode(api.store.runMode === 'running' ? 'stopped' : 'running')}
			>
				{#if api.store.runMode === 'running'}
					<Pause class="size-3.5" /> Pause
				{:else}
					<Play class="size-3.5" /> Start
				{/if}
			</Button>
			<Button variant="outline" size="sm" onclick={() => api.store.evaluate()}>
				<RotateCcw class="size-3.5" /> Eval
			</Button>
			<Button variant="ghost" size="icon-sm" onclick={api.refreshNets}>
				<RefreshCw class="size-3.5" />
			</Button>
		</div>
	</div>
{/snippet}

<!-- Full-bleed canvas page (PageShell `bleed`): NetWorkbench owns the whole
     layout (toolbar band via the `header` snippet, net tree, xyflow canvas,
     side panels) — no width container or scroll wrapper here. -->
<PageShell width="bleed">
	<NetWorkbench {netId} onDeleteNet={handleDeleteNet} {header} />
</PageShell>
