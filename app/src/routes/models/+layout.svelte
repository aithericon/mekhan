<script lang="ts">
	// The self-hosted LLM control plane (docs/28-31), promoted to its own page.
	// A URL-routed tab bar splits the pool + router into focused subpages — each
	// is a proper subroute (navigation unmounts the previous view), so tabs are
	// linkable/bookmarkable. Inference bypasses the engine net entirely (the HTTP
	// router meters directly); this page is the control surface, never inference.
	import { PageShell, PageHeader, PageTabs, type PageTab } from '$lib/components/shell';
	import Cpu from '@lucide/svelte/icons/cpu';
	import LibraryBig from '@lucide/svelte/icons/library-big';
	import Boxes from '@lucide/svelte/icons/boxes';
	import Activity from '@lucide/svelte/icons/activity';

	let { children } = $props();

	// Ordered to follow the operator workflow: discover a model (Catalog) →
	// curate + run it (Set) → watch it live (Engines) → observe inference
	// (Telemetry). Autoscaling places models across registered runners — there
	// is no node provisioning, so no separate capacity tab.
	const tabs: PageTab[] = [
		{
			href: '/models/catalog',
			label: 'Catalog',
			icon: LibraryBig,
			title: 'Browse the Ollama library + Hugging Face, provision onto a runner',
			testid: 'model-pool-tab-catalog'
		},
		{
			href: '/models/set',
			label: 'Set',
			icon: Boxes,
			title: 'The operator-curated model set + lifecycle — load / unload / autoscale',
			testid: 'model-pool-tab-set'
		},
		{
			href: '/models/engines',
			label: 'Engines',
			icon: Cpu,
			title: 'Live per-node engine inventory — what is resident on each runner',
			testid: 'model-pool-tab-engines'
		},
		{
			href: '/models/router',
			label: 'Telemetry',
			icon: Activity,
			title: 'Per-model Prometheus metrics + the inference audit ledger (metering / GDPR)',
			testid: 'model-pool-tab-router'
		}
	];
</script>

<PageShell width="wide" testid="model-pool-page">
	{#snippet band()}
		<PageHeader
			title="Model Pool"
			subtitle="Self-hosted LLM serving — operator curates the model set, the autoscaler manages count + placement. Inference bypasses the engine net (HTTP router)."
			class="mb-3"
		/>
		<PageTabs testid="model-pool-tabs" {tabs} />
	{/snippet}
	{@render children()}
</PageShell>
