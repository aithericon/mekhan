<script lang="ts">
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import { Spinner } from '$lib/components/ui/spinner';
	import { CopyButton } from '$lib/components/ui/copy-button';
	import { getFolderOpenApiBundle } from '$lib/api/client';
	import { parseBundle, type ParsedBundle, type Endpoint } from '$lib/api/openapi-bundle';
	import TriggerInvokePanel from './TriggerInvokePanel.svelte';
	import Shield from '@lucide/svelte/icons/shield';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import ArrowUpRight from '@lucide/svelte/icons/arrow-up-right';

	type Props = {
		workspaceId: string;
		folderId: string;
	};
	let { workspaceId, folderId }: Props = $props();

	let loading = $state(false);
	let error = $state<string | null>(null);
	let bundle = $state<ParsedBundle | null>(null);
	let rawUrl = $derived(`/api/v1/workspaces/${workspaceId}/folders/${folderId}/openapi.json`);
	let expanded = $state<Record<string, boolean>>({});

	// Re-fetch whenever the target folder changes.
	let loadedFor = $state<string | null>(null);
	$effect(() => {
		if (loadedFor === folderId && bundle) return;
		loading = true;
		error = null;
		getFolderOpenApiBundle(workspaceId, folderId)
			.then((doc) => {
				bundle = parseBundle(doc);
				loadedFor = folderId;
			})
			.catch((e) => {
				error = e instanceof Error ? e.message : 'Failed to load API bundle';
				bundle = null;
			})
			.finally(() => (loading = false));
	});

	function key(ep: Endpoint): string {
		return ep.kind === 'run' ? `run:${ep.templateId}` : `${ep.kind}:${ep.nodeId}`;
	}
	function toggle(ep: Endpoint) {
		const k = key(ep);
		expanded = { ...expanded, [k]: !expanded[k] };
	}

	const manualCount = $derived(bundle?.endpoints.filter((e) => e.kind === 'manual').length ?? 0);
	const webhookCount = $derived(bundle?.endpoints.filter((e) => e.kind === 'webhook').length ?? 0);
	const runCount = $derived(bundle?.endpoints.filter((e) => e.kind === 'run').length ?? 0);
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
			<Badge variant="secondary">{runCount} template{runCount === 1 ? '' : 's'}</Badge>
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
				No callable contracts. Publish a template homed in this folder (or any
				descendant) — each published template is exposed as a runnable endpoint, plus
				a dedicated path per enabled Manual or Webhook trigger.
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
							{:else if ep.kind === 'run'}
								<Badge variant="secondary" class="text-xs">run</Badge>
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
						{:else if ep.kind === 'webhook'}
							<div class="font-mono text-xs text-muted-foreground">
								<span class="font-semibold text-foreground">{ep.method}</span> {ep.path}
							</div>
						{:else if ep.kind === 'run'}
							<div class="font-mono text-xs text-muted-foreground">
								<span class="font-semibold text-foreground">POST</span> /api/v1/instances
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
				{:else if ep.kind === 'run'}
					<div class="space-y-2 border-t border-border/60 px-3 py-2.5 text-xs text-muted-foreground">
						<p>
							Launch this template via
							<code>POST /api/v1/instances</code> with
							<code>template_id</code> = <code class="break-all">{ep.templateId}</code>.
						</p>
						{#each ep.startBlocks as sb (sb.startBlockId)}
							<div class="rounded border border-border/40 px-2 py-1.5">
								<div class="font-mono text-[11px] text-foreground/80">
									start_tokens[] · <span class="text-muted-foreground">{sb.startBlockId}</span>
								</div>
								{#if sb.fields.length > 0}
									<ul class="mt-1 space-y-0.5">
										{#each sb.fields as f (f.name)}
											<li class="font-mono text-[11px]">
												<span class="text-foreground">{f.name}</span><span class="text-muted-foreground"
													>: {f.type}{f.format ? ` (${f.format})` : ''}{f.required ? '' : '?'}</span
												>
											</li>
										{/each}
									</ul>
								{:else}
									<div class="mt-1 text-[11px] text-muted-foreground/70">No input fields.</div>
								{/if}
							</div>
						{/each}
					</div>
				{/if}
			</div>
		{/each}
	{/if}
</div>
