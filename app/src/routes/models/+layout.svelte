<script lang="ts">
	// The self-hosted LLM control plane (docs/28-31), promoted to its own page.
	// A URL-routed tab bar splits the pool + router into focused subpages — each
	// is a proper subroute (navigation unmounts the previous view), so tabs are
	// linkable/bookmarkable. Inference bypasses the engine net entirely (the HTTP
	// router meters directly); this page is the control surface, never inference.
	import { page } from '$app/state';
	import Cpu from '@lucide/svelte/icons/cpu';
	import LibraryBig from '@lucide/svelte/icons/library-big';
	import Boxes from '@lucide/svelte/icons/boxes';
	import Network from '@lucide/svelte/icons/network';
	import Activity from '@lucide/svelte/icons/activity';

	let { children } = $props();

	const pathname = $derived(page.url.pathname);

	type TabDef = { href: string; match: string; label: string; icon: typeof Cpu; title: string };
	const tabs: TabDef[] = [
		{
			href: '/models/engines',
			match: 'engines',
			label: 'Engines',
			icon: Cpu,
			title: 'Live per-node inventory — load / unload / pull, ready-to-load'
		},
		{
			href: '/models/catalog',
			match: 'catalog',
			label: 'Catalog',
			icon: LibraryBig,
			title: 'Browse the Ollama library + Hugging Face, provision onto a runner'
		},
		{
			href: '/models/set',
			match: 'set',
			label: 'Set',
			icon: Boxes,
			title: 'The operator-curated model set + lifecycle'
		},
		{
			href: '/models/placement',
			match: 'placement',
			label: 'Placement',
			icon: Network,
			title: 'Placement policies + node pools (the autoscaler rows)'
		},
		{
			href: '/models/router',
			match: 'router',
			label: 'Router',
			icon: Activity,
			title: 'Inference audit ledger (metering / GDPR)'
		}
	];

	const isActive = (match: string) => pathname.startsWith(`/models/${match}`);
</script>

<svelte:head><title>Model Pool | Mekhan</title></svelte:head>

<div class="flex h-full flex-col" data-testid="model-pool-page">
	<div class="border-b border-border bg-card px-6 pt-5 shrink-0">
		<div class="mx-auto max-w-6xl">
			<h1 class="text-xl font-semibold tracking-tight text-foreground">Model Pool</h1>
			<p class="mt-0.5 text-base text-muted-foreground">
				Self-hosted LLM serving — operator curates the model set, the autoscaler manages count +
				placement. Inference bypasses the engine net (HTTP router).
			</p>
			<nav class="mt-3 flex items-center gap-1" data-testid="model-pool-tabs">
				{#each tabs as tab (tab.match)}
					{@const active = isActive(tab.match)}
					{@const Icon = tab.icon}
					<a
						href={tab.href}
						class="inline-flex items-center gap-1.5 rounded-t-md border-b-2 px-2.5 py-1.5 text-sm font-medium transition-colors
							{active
							? 'border-primary text-foreground'
							: 'border-transparent text-muted-foreground hover:text-foreground'}"
						title={tab.title}
						data-testid="model-pool-tab-{tab.match}"
						aria-current={active ? 'page' : undefined}
					>
						<Icon class="size-3.5" />
						{tab.label}
					</a>
				{/each}
			</nav>
		</div>
	</div>

	<div class="flex-1 overflow-y-auto">
		<div class="mx-auto max-w-6xl px-6 py-6 animate-rise">
			{@render children()}
		</div>
	</div>
</div>
