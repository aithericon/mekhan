<script lang="ts">
	import { page } from '$app/stores';
	import {
		updateProcess,
		getInstance,
		type WorkflowInstance,
		type ProcessDetail
	} from '$lib/api/client';
	import { ProcessView } from '$lib/components/processes';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { Separator } from '$lib/components/ui/separator';
	import ArrowLeft from '@lucide/svelte/icons/arrow-left';
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

	const statusColors: Record<string, string> = {
		active: 'bg-green-100 text-green-800 dark:bg-green-900 dark:text-green-200',
		completed: 'bg-blue-100 text-blue-800 dark:bg-blue-900 dark:text-blue-200',
		failed: 'bg-red-100 text-red-800 dark:bg-red-900 dark:text-red-200'
	};
	const kindColors: Record<string, string> = {
		'petri-net': 'bg-purple-100 text-purple-800 dark:bg-purple-900 dark:text-purple-200',
		'bo-campaign': 'bg-orange-100 text-orange-800 dark:bg-orange-900 dark:text-orange-200',
		pipeline: 'bg-cyan-100 text-cyan-800 dark:bg-cyan-900 dark:text-cyan-200'
	};
	function statusColor(s: string): string {
		return (
			statusColors[s.toLowerCase()] ??
			'bg-slate-100 text-slate-700 dark:bg-slate-800 dark:text-slate-300'
		);
	}
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

<div class="h-full overflow-y-auto">
	<div class="mx-auto w-full px-6 py-8 animate-rise">
		<!-- Back link -->
		<a
			href="/processes"
			class="mb-6 inline-flex items-center gap-1.5 text-sm text-muted-foreground transition-colors hover:text-foreground"
		>
			<ArrowLeft class="size-4" />
			Back to processes
		</a>

		{#if detail}
			<!-- Header -->
			<div class="mb-6">
				<div class="flex items-center gap-2 mb-2">
					{#if editingName}
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
					{:else}
						<h1 class="text-2xl font-semibold tracking-tight text-foreground">
							{detail.name ?? 'Unnamed Process'}
						</h1>
						<button
							class="text-muted-foreground hover:text-foreground transition-colors"
							onclick={() => {
								editNameValue = detail?.name ?? '';
								editingName = true;
							}}
						>
							<Pencil class="size-4" />
						</button>
					{/if}
				</div>

				<div class="flex flex-wrap items-center gap-2 mb-2">
					<Badge class={statusColor(detail.status)} variant="secondary">
						{detail.status}
					</Badge>
					{#if detail.kind}
						<Badge class={kindColor(detail.kind)} variant="secondary">
							{detail.kind}
						</Badge>
					{/if}
					{#if detail.owner}
						<span class="text-sm text-muted-foreground">Owner: {detail.owner}</span>
					{/if}
				</div>

				<p class="font-mono text-sm text-muted-foreground mb-1">{detail.process_id}</p>
				{#if detail.instance_id}
					<div class="mb-1 flex flex-wrap items-center gap-1.5 text-sm">
						<span class="text-muted-foreground">Origin:</span>
						<a
							href="/instances/{detail.instance_id}"
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
					<span>Created {formatDate(detail.created_at)}</span>
					<span>Updated {relativeTime(detail.updated_at)}</span>
				</div>
			</div>

			<Separator class="mb-4" />
		{/if}

		<ProcessView {processId} bind:detail />
	</div>
</div>
