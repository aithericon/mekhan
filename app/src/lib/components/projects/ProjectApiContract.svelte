<script lang="ts">
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import { Spinner } from '$lib/components/ui/spinner';
	import { CopyButton } from '$lib/components/ui/copy-button';
	import { getProjectOpenApiBundle } from '$lib/api/client';
	import { parseBundle, type ParsedBundle, type Endpoint } from '$lib/api/openapi-bundle';
	import TriggerInvokePanel from './TriggerInvokePanel.svelte';
	import Shield from '@lucide/svelte/icons/shield';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import ArrowUpRight from '@lucide/svelte/icons/arrow-up-right';

	type Props = {
		workspaceId: string;
		projectId: string;
	};
	let { workspaceId, projectId }: Props = $props();

	let loading = $state(false);
	let error = $state<string | null>(null);
	let bundle = $state<ParsedBundle | null>(null);
	let rawUrl = $derived(`/api/v1/workspaces/${workspaceId}/projects/${projectId}/openapi.json`);
	let expanded = $state<Record<string, boolean>>({});

	// Re-fetch whenever the target project changes.
	let loadedFor = $state<string | null>(null);
	$effect(() => {
		if (loadedFor === projectId && bundle) return;
		loading = true;
		error = null;
		getProjectOpenApiBundle(workspaceId, projectId)
			.then((doc) => {
				bundle = parseBundle(doc);
				loadedFor = projectId;
			})
			.catch((e) => {
				error = e instanceof Error ? e.message : 'Failed to load API bundle';
				bundle = null;
			})
			.finally(() => (loading = false));
	});

	function key(ep: Endpoint): string {
		return `${ep.kind}:${ep.nodeId}`;
	}
	function toggle(ep: Endpoint) {
		const k = key(ep);
		expanded = { ...expanded, [k]: !expanded[k] };
	}

	const manualCount = $derived(bundle?.endpoints.filter((e) => e.kind === 'manual').length ?? 0);
	const webhookCount = $derived(bundle?.endpoints.filter((e) => e.kind === 'webhook').length ?? 0);
</script>

<div class="space-y-4">
	{#if loading}
		<div class="flex items-center gap-2 text-sm text-muted-foreground">
			<Spinner class="size-4" /> Loading contract…
		</div>
	{:else if error}
		<div class="rounded-md border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800">
			{error}
		</div>
	{:else if bundle}
		<!-- Spec link + counts -->
		<div class="flex flex-wrap items-center gap-2 text-sm">
			<Badge variant="secondary">{manualCount} callable</Badge>
			<Badge variant="secondary">{webhookCount} webhook{webhookCount === 1 ? '' : 's'}</Badge>
			<a
				href={rawUrl}
				target="_blank"
				rel="noopener"
				class="ml-auto inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
			>
				openapi.json <ArrowUpRight class="size-3.5" />
			</a>
			<CopyButton text={typeof window !== 'undefined' ? `${window.location.origin}${rawUrl}` : rawUrl} />
		</div>

		{#if bundle.securitySchemes.length > 0}
			<div class="flex flex-wrap items-center gap-2">
				<Shield class="size-3.5 text-muted-foreground" />
				{#each bundle.securitySchemes as s (s.name)}
					<span class="rounded-md bg-muted/40 px-2 py-0.5 text-xs text-muted-foreground">
						{s.name} <span class="text-muted-foreground/70">· {s.detail}</span>
					</span>
				{/each}
			</div>
		{/if}

		{#if bundle.endpoints.length === 0}
			<p class="text-sm text-muted-foreground">
				No callable triggers. Add an enabled Manual or Webhook trigger to a published
				template attached to this project.
			</p>
		{/if}

		{#each bundle.endpoints as ep (key(ep))}
			<div class="rounded-lg border border-border/60">
				<div class="flex items-start justify-between gap-2 px-3 py-2.5">
					<div class="min-w-0 space-y-1">
						<div class="flex items-center gap-2">
							<span class="text-sm font-medium">{ep.title}</span>
							{#if ep.templateName}
								<Badge variant="outline" class="text-xs">{ep.templateName}</Badge>
							{/if}
							{#if ep.kind === 'manual' && !ep.typed}
								<Badge variant="secondary" class="text-xs">loose body</Badge>
							{/if}
						</div>
						{#if ep.kind === 'manual'}
							<div class="flex flex-wrap gap-1.5 font-mono text-xs text-muted-foreground">
								{#if ep.firePath}
									<span><span class="font-semibold text-foreground">POST</span> {ep.firePath}</span>
								{/if}
								{#if ep.invokePath}
									<span class="text-muted-foreground/50">·</span>
									<span><span class="font-semibold text-foreground">POST</span> {ep.invokePath}</span>
								{/if}
							</div>
						{:else}
							<div class="font-mono text-xs text-muted-foreground">
								<span class="font-semibold text-foreground">{ep.method}</span> {ep.path}
							</div>
						{/if}
					</div>
					{#if ep.kind === 'manual'}
						<Button
							variant="ghost"
							size="sm"
							data-testid={`btn-tryit-${ep.nodeId}`}
							onclick={() => toggle(ep)}
						>
							Try it
							<ChevronDown class="size-3.5 transition-transform {expanded[key(ep)] ? 'rotate-180' : ''}" />
						</Button>
					{/if}
				</div>

				{#if ep.kind === 'manual' && expanded[key(ep)]}
					<div class="border-t border-border/60 px-3 py-3">
						<TriggerInvokePanel endpoint={ep} />
					</div>
				{:else if ep.kind === 'webhook'}
					<div class="border-t border-border/60 px-3 py-2 text-xs text-muted-foreground">
						Async webhook receiver — accepts a free-form JSON body (projected by the
						trigger's payload mapping). Returns <code>202</code>.
					</div>
				{/if}
			</div>
		{/each}
	{/if}
</div>
