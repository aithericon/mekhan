<script lang="ts">
	import { page } from '$app/stores';
	import {
		updateProcess,
		getInstance,
		type WorkflowInstance,
		type ProcessDetail
	} from '$lib/api/client';
	import { PageShell, PageHeader } from '$lib/components/shell';
	import { ProcessView } from '$lib/components/processes';
	import { Badge } from '$lib/components/ui/badge';
	import { StatusBadge } from '$lib/components/status';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { Separator } from '$lib/components/ui/separator';
	import ChevronLeft from '@lucide/svelte/icons/chevron-left';
	import Pencil from '@lucide/svelte/icons/pencil';
	import Check from '@lucide/svelte/icons/check';
	import X from '@lucide/svelte/icons/x';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';

	// Bound from ProcessView (single source of the getProcess fetch).
	let detail = $state<ProcessDetail | null>(null);
	let linkedInstance = $state<WorkflowInstance | null>(null);

	let editingName = $state(false);
	let editNameValue = $state('');

	const processId = $derived(($page.params as Record<string, string>).process_id);

	const kindColors: Record<string, string> = {
		'petri-net': 'bg-purple-100 text-purple-800 dark:bg-purple-900 dark:text-purple-200',
		'bo-campaign': 'bg-orange-100 text-orange-800 dark:bg-orange-900 dark:text-orange-200',
		pipeline: 'bg-cyan-100 text-cyan-800 dark:bg-cyan-900 dark:text-cyan-200'
	};
	function kindColor(k: string): string {
		return (
			kindColors[k.toLowerCase()] ??
			'bg-slate-100 text-slate-700 dark:bg-slate-800 dark:text-slate-300'
		);
	}

	const formatDate = (s: string) =>
		new Intl.DateTimeFormat(undefined, {
			year: 'numeric',
			month: 'short',
			day: 'numeric',
			hour: '2-digit',
			minute: '2-digit'
		}).format(new Date(s));

	function relativeTime(dateStr: string): string {
		const now = Date.now();
		const then = new Date(dateStr).getTime();
		const diff = now - then;
		if (diff < 60_000) return 'just now';
		if (diff < 3600_000) return `${Math.floor(diff / 60_000)}m ago`;
		if (diff < 86400_000) return `${Math.floor(diff / 3600_000)}h ago`;
		return new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric' }).format(
			new Date(dateStr)
		);
	}

	async function saveName() {
		if (!detail) return;
		try {
			await updateProcess(processId, { name: editNameValue });
			detail = { ...detail, name: editNameValue };
			editingName = false;
		} catch {
			// Silently fail — user can retry
		}
	}

	// Resolve the run lineage (Origin row) from the loaded process.
	$effect(() => {
		const iid = detail?.instance_id;
		if (!iid) {
			linkedInstance = null;
			return;
		}
		let cancelled = false;
		getInstance(iid)
			.then((i) => {
				if (!cancelled) linkedInstance = i;
			})
			.catch(() => {
				if (!cancelled) linkedInstance = null;
			});
		return () => {
			cancelled = true;
		};
	});
</script>

<PageShell width="full">
	{#snippet band()}
		{#if detail}
			{@const d = detail}
			<!-- Meta rows shared between display + rename header states -->
			{#snippet metaRows()}
				<div class="mt-2 mb-2 flex flex-wrap items-center gap-2">
					<StatusBadge domain="process" status={d.status} />
					{#if d.kind}
						<Badge class={kindColor(d.kind)} variant="secondary">
							{d.kind}
						</Badge>
					{/if}
					{#if d.owner}
						<span class="text-sm text-muted-foreground">Owner: {d.owner}</span>
					{/if}
				</div>

				<p class="font-mono text-sm text-muted-foreground mb-1">{d.process_id}</p>
				{#if d.instance_id}
					<div class="mb-1 flex flex-wrap items-center gap-1.5 text-sm">
						<span class="text-muted-foreground">Origin:</span>
						<a
							href="/instances/{d.instance_id}"
							class="inline-flex items-center gap-1 text-primary hover:underline"
							data-testid="process-instance-link"
						>
							Instance
							<ChevronRight class="size-3" />
						</a>
						{#if linkedInstance}
							<a
								href="/templates/{linkedInstance.template_id}"
								class="inline-flex items-center gap-1 text-primary hover:underline"
								data-testid="process-template-link"
							>
								Template v{linkedInstance.template_version}
								<ChevronRight class="size-3" />
							</a>
						{/if}
					</div>
				{/if}
				<div class="flex items-center gap-4 text-sm text-muted-foreground">
					<span>Created {formatDate(d.created_at)}</span>
					<span>Updated {relativeTime(d.updated_at)}</span>
				</div>
		{/snippet}

		{#if editingName}
			<!-- Inline-rename state: the title is an input, so PageHeader (string
			     title) steps aside for this transient editing header. -->
			<header>
				<div class="mb-3">
					<a
						href="/processes"
						class="inline-flex items-center gap-1 text-sm text-muted-foreground transition-colors hover:text-foreground"
					>
						<ChevronLeft class="size-4" />
						Back to processes
					</a>
				</div>
				<div class="flex items-center gap-2">
					<Input
						type="text"
						class="h-8 w-64 text-sm"
						bind:value={editNameValue}
						onkeydown={(e: KeyboardEvent) => {
							if (e.key === 'Enter') saveName();
							if (e.key === 'Escape') editingName = false;
						}}
					/>
					<Button variant="ghost" size="icon-sm" onclick={saveName}>
						<Check class="size-4" />
					</Button>
					<Button variant="ghost" size="icon-sm" onclick={() => (editingName = false)}>
						<X class="size-4" />
					</Button>
				</div>
				{@render metaRows()}
			</header>
		{:else}
			<PageHeader
				title={d.name ?? 'Unnamed Process'}
				variant="detail"
				back={{ href: '/processes', label: 'Back to processes' }}
			>
				{#snippet actions()}
					<Button
						variant="ghost"
						size="icon-sm"
						onclick={() => {
							editNameValue = d.name ?? '';
							editingName = true;
						}}
					>
						<Pencil class="size-4" />
					</Button>
				{/snippet}
				{#snippet children()}
					{@render metaRows()}
				{/snippet}
			</PageHeader>
			{/if}
		{:else}
			<!-- Back link while the process is still loading -->
			<a
				href="/processes"
				class="inline-flex items-center gap-1 text-sm text-muted-foreground transition-colors hover:text-foreground"
			>
				<ChevronLeft class="size-4" />
				Back to processes
			</a>
		{/if}
	{/snippet}

	<ProcessView {processId} bind:detail />
</PageShell>
