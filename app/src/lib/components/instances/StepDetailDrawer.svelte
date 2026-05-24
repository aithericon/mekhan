<script lang="ts">
	import { Sheet, SheetContent, SheetTitle, SheetDescription, SheetClose } from '$lib/components/ui/sheet';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import { Dialog } from 'bits-ui';
	import X from '@lucide/svelte/icons/x';
	import Settings2 from '@lucide/svelte/icons/settings-2';
	import type { StepExecution, WorkflowNode } from '$lib/api/client';
	import { nodeKindMeta } from './node-kind-meta';
	import { SmartValue } from './output-renderers';

	type Props = {
		step: StepExecution | null;
		/** The node from the template graph this step instantiates. Optional —
		 *  the drawer degrades gracefully when not supplied (e.g. older callers). */
		node?: WorkflowNode | null;
		/** Every iteration of this node in the current instance (oldest → newest).
		 *  Drives the iteration picker for Loop bodies. When omitted or
		 *  single-element, the picker is hidden. */
		iterations?: StepExecution[];
		/** Owning workflow instance id. Forwarded into the renderer context so
		 *  envelope renderers can resolve instance-scoped backend resources
		 *  (e.g. AutomatedStepEnvelope's log lookup). */
		instanceId?: string;
		open: boolean;
		onClose: () => void;
		/** When the user picks a different iteration in the drawer, the parent
		 *  swaps `step` for the chosen row. */
		onSelectIteration?: (iterationIndex: number) => void;
	};

	let {
		step,
		node = null,
		iterations = [],
		instanceId,
		open,
		onClose,
		onSelectIteration
	}: Props = $props();

	const statusColor: Record<string, string> = {
		pending: 'bg-gray-100 text-gray-700',
		running: 'bg-blue-100 text-blue-700',
		completed: 'bg-green-100 text-green-700',
		failed: 'bg-red-100 text-red-700',
		skipped: 'bg-slate-100 text-slate-500'
	};

	// The node kind from the template's WorkflowNode (`type` is the AIR-side
	// kind, also snake_case) or from the step row (snake_case from the
	// projection). Prefer the step's kind since that's what the projection
	// actually saw; fall back to the template node when the step is unavailable.
	const kind = $derived(step?.node_kind ?? node?.type ?? 'unknown');
	const meta = $derived(nodeKindMeta(kind));
	const Icon = $derived(meta.icon);

	const nodeLabel = $derived<string>(
		(node?.data?.label ?? '') || step?.node_id || 'Step'
	);
	const nodeDescription = $derived<string | null>(
		node?.data?.description ?? null
	);

	let configOpen = $state(false);

	function formatDuration(ms: number | null | undefined): string {
		if (ms === null || ms === undefined) return '—';
		if (ms < 1000) return `${ms}ms`;
		if (ms < 60_000) return `${(ms / 1000).toFixed(2)}s`;
		const mins = Math.floor(ms / 60_000);
		const secs = Math.floor((ms % 60_000) / 1000);
		return `${mins}m ${secs}s`;
	}

	function pretty(json: unknown): string {
		try {
			return JSON.stringify(json, null, 2);
		} catch {
			return String(json);
		}
	}

	function inputEntries(inputs: unknown): Array<[string, unknown]> {
		if (!inputs || typeof inputs !== 'object') return [];
		return Object.entries(inputs as Record<string, unknown>);
	}
</script>

<Sheet.Root bind:open onOpenChange={(v: boolean) => { if (!v) onClose(); }}>
	<SheetContent class="w-full sm:max-w-xl">
		{#if step}
			<!-- SheetTitle/SheetDescription are sr-only by design (a11y).
			     The visible header below mirrors them. -->
			<SheetTitle>{nodeLabel} — {meta.label} ({step.status})</SheetTitle>
			<SheetDescription>
				Runtime detail for step {step.node_id}{step.iteration_index > 0 ? `, iteration ${step.iteration_index}` : ''}.
			</SheetDescription>

			<header class="flex items-start gap-3 border-b border-border px-5 py-4">
				<!-- Mirror the canvas card: kind-coloured icon chip + label. -->
				<div class="flex size-9 shrink-0 items-center justify-center rounded-md {meta.chipClass}">
					<Icon class="size-5 {meta.iconClass}" />
				</div>

				<div class="min-w-0 flex-1">
					<h2 class="text-base font-semibold text-foreground truncate">
						{nodeLabel}
					</h2>
					<div class="mt-1 flex flex-wrap items-center gap-2 text-sm">
						<Badge variant="outline" class="font-mono">{meta.label}</Badge>
						<Badge class={statusColor[step.status] ?? ''} variant="secondary">
							{step.status}
						</Badge>
						{#if step.iteration_index > 0 || iterations.length > 1}
							<Badge variant="outline">iter {step.iteration_index}</Badge>
						{/if}
						{#if step.branch_taken}
							<Badge variant="outline" class="font-mono">→ {step.branch_taken}</Badge>
						{/if}
					</div>
					<div class="mt-1 font-mono text-sm text-muted-foreground/80 truncate" title={step.node_id}>
						id: {step.node_id}
					</div>
					{#if nodeDescription}
						<p class="mt-1 text-sm text-muted-foreground line-clamp-2">{nodeDescription}</p>
					{/if}
				</div>

				<div class="flex shrink-0 items-center gap-1">
					{#if node}
						<Button
							variant="ghost"
							size="sm"
							onclick={() => (configOpen = true)}
							title="View the node's saved configuration"
						>
							<Settings2 class="size-4" />
							<span class="ml-1.5 hidden sm:inline">Config</span>
						</Button>
					{/if}
					<SheetClose>
						<Button variant="ghost" size="icon" aria-label="Close">
							<X class="size-4" />
						</Button>
					</SheetClose>
				</div>
			</header>

			<div class="flex-1 overflow-y-auto px-5 py-4 space-y-5">
				{#if iterations.length > 1 && onSelectIteration}
					<section>
						<h3 class="text-sm font-semibold text-foreground mb-2">Iterations</h3>
						<div class="flex flex-wrap gap-1">
							{#each iterations as it (it.iteration_index)}
								<button
									class="rounded-md border px-2 py-1 text-sm font-mono transition-colors
										{step.iteration_index === it.iteration_index
											? 'border-primary bg-primary/10 text-foreground'
											: 'border-border text-muted-foreground hover:bg-accent hover:text-foreground'}"
									onclick={() => onSelectIteration(it.iteration_index)}
								>
									#{it.iteration_index}
								</button>
							{/each}
						</div>
					</section>
				{/if}

				<section class="grid grid-cols-3 gap-3 text-sm">
					<div>
						<div class="text-muted-foreground">Started</div>
						<div class="font-medium">
							{step.started_at ? new Date(step.started_at).toLocaleTimeString() : '—'}
						</div>
					</div>
					<div>
						<div class="text-muted-foreground">Completed</div>
						<div class="font-medium">
							{step.completed_at ? new Date(step.completed_at).toLocaleTimeString() : '—'}
						</div>
					</div>
					<div>
						<div class="text-muted-foreground">Duration</div>
						<div class="font-medium">{formatDuration(step.duration_ms)}</div>
					</div>
				</section>

				{#if step.error}
					<section>
						<h3 class="text-sm font-semibold text-destructive mb-2">Error</h3>
						<pre class="rounded-md border border-destructive/30 bg-destructive/5 p-3 text-sm font-mono whitespace-pre-wrap break-words text-destructive">{pretty(step.error)}</pre>
					</section>
				{/if}

				<section>
					<h3 class="text-sm font-semibold text-foreground mb-2">
						Inputs
						{#if inputEntries(step.inputs).length === 0}
							<span class="ml-1 text-sm font-normal text-muted-foreground">— none</span>
						{/if}
					</h3>
					{#if inputEntries(step.inputs).length > 0}
						<div class="space-y-3">
							{#each inputEntries(step.inputs) as [producerNode, envelope] (producerNode)}
								<div>
									<div class="mb-1 text-sm font-mono text-muted-foreground">
										from <span class="text-foreground">{producerNode}</span>
									</div>
									<!-- nodeKind is left undefined: `producerNode` is a slug,
									     not a kind. The registry's shape predicates are specific
									     enough to dispatch on shape alone. -->
									<SmartValue value={envelope} ctx={{ position: 'input', instanceId }} />
								</div>
							{/each}
						</div>
					{/if}
				</section>

				<section>
					<h3 class="text-sm font-semibold text-foreground mb-2">
						Outputs
						{#if step.outputs === null || step.outputs === undefined}
							<span class="ml-1 text-sm font-normal text-muted-foreground">— none</span>
						{/if}
					</h3>
					{#if step.outputs !== null && step.outputs !== undefined}
						<SmartValue
							value={step.outputs}
							ctx={{ position: 'output', nodeKind: step.node_kind, instanceId }}
						/>
					{/if}
				</section>
			</div>
		{/if}
	</SheetContent>
</Sheet.Root>

<!-- Config inspector: layered above the sheet so the runtime view stays
     visible underneath. Read-only — this is a debugging/audit lens, not an
     editor; the template editor is the place to change node config. -->
<Dialog.Root bind:open={configOpen}>
	<Dialog.Portal>
		<Dialog.Overlay class="fixed inset-0 z-[60] bg-black/40 data-[state=open]:animate-in data-[state=open]:fade-in" />
		<Dialog.Content
			class="fixed left-1/2 top-1/2 z-[70] flex max-h-[85vh] w-[min(90vw,720px)] -translate-x-1/2 -translate-y-1/2 flex-col rounded-lg border border-border bg-card shadow-xl data-[state=open]:animate-in data-[state=open]:fade-in data-[state=open]:zoom-in-95"
		>
			<header class="flex items-start gap-3 border-b border-border px-5 py-3">
				<div class="flex size-7 shrink-0 items-center justify-center rounded-md {meta.chipClass}">
					<Icon class="size-4 {meta.iconClass}" />
				</div>
				<div class="min-w-0 flex-1">
					<Dialog.Title class="text-base font-semibold text-foreground">
						{nodeLabel} — configuration
					</Dialog.Title>
					<Dialog.Description class="text-sm text-muted-foreground">
						Read-only view of the node as published with the template.
					</Dialog.Description>
				</div>
				<Dialog.Close>
					<Button variant="ghost" size="icon" aria-label="Close">
						<X class="size-4" />
					</Button>
				</Dialog.Close>
			</header>

			<div class="flex-1 overflow-y-auto px-5 py-4">
				{#if node}
					<pre class="rounded-md border border-border bg-muted/30 p-3 text-sm font-mono whitespace-pre-wrap break-words">{pretty(node)}</pre>
				{:else}
					<p class="text-sm text-muted-foreground">Configuration not available for this node.</p>
				{/if}
			</div>
		</Dialog.Content>
	</Dialog.Portal>
</Dialog.Root>
