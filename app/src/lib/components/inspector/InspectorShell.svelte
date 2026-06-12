<script lang="ts">
	/**
	 * Shared chrome for the platform's two node inspectors so edit-mode and
	 * run-mode feel like one surface:
	 *   - the editor's `NodePropertyPanel` (a right-side, Yjs-bound, EDITABLE panel),
	 *   - the instance view's `StepDetailDrawer` (a Sheet, READ-ONLY runtime view).
	 *
	 * This component is PURELY presentational: it renders the identity header
	 * (kind-coloured icon chip + label + optional id chip + status/actions slots
	 * + close) and a description line, then yields a scrollable body. It owns no
	 * data fetching, no Yjs, no close mechanics — the close affordance is a
	 * caller-supplied snippet because the editor uses a plain button and the
	 * drawer uses `SheetClose`. Node identity (icon/label/colours) is sourced
	 * from `node-kind-meta.ts`, the single source of truth shared with the
	 * canvas card — killing the editor panel's old hand-rolled identity header.
	 */
	import type { Snippet } from 'svelte';
	import { Badge } from '$lib/components/ui/badge';
	import { nodeKindMeta, normalizeNodeKind } from '$lib/components/instances/node-kind-meta';

	type Props = {
		/** Raw node-kind discriminant from either surface — `WorkflowNodeData.type`
		 *  (editor), `StepExecution.node_kind` or `WorkflowNode.type` (instance).
		 *  Normalized internally to the shared meta key. */
		kind: string | null | undefined;
		/** Author/runtime label shown next to the icon. */
		label: string;
		/** Optional node id; rendered as a monospaced `id: …` line when present. */
		nodeId?: string | null;
		/** Optional description line under the badges. */
		description?: string | null;
		/** Whether the header/body should scroll (drawer) — the editor panel keeps
		 *  its own outer width frame, so it passes its own classes via `frameClass`. */
		frameClass?: string;
		/** Padding/spacing classes for the header row. Defaults suit the drawer;
		 *  the editor passes its tighter spacing. */
		headerClass?: string;
		/** Padding/spacing classes for the scrollable body region. */
		bodyClass?: string;
		/** Badges shown to the right of the kind label (status, iteration, …). */
		status?: Snippet;
		/** Buttons rendered in the top-right action cluster (Copy/Config/Delete). */
		actions?: Snippet;
		/** The close affordance — a plain button (editor) or `SheetClose` (drawer). */
		close?: Snippet;
		/** `data-testid` for the outer frame. Defaults to `inspector-shell`; the
		 *  editor panel overrides it with its historical `node-property-panel` so
		 *  existing Playwright selectors keep working. */
		testid?: string;
		/** Inspector body. */
		children: Snippet;
	};

	let {
		kind,
		label,
		nodeId = null,
		description = null,
		frameClass = 'flex h-full flex-col',
		headerClass = 'flex items-start gap-3 border-b border-border px-5 py-4',
		bodyClass = 'flex-1 overflow-y-auto px-5 py-4 space-y-5',
		testid = 'inspector-shell',
		status,
		actions,
		close,
		children
	}: Props = $props();

	const meta = $derived(nodeKindMeta(normalizeNodeKind(kind)));
	const Icon = $derived(meta.icon);
</script>

<div class={frameClass} data-testid={testid}>
	<header class={headerClass}>
		<!-- Kind-coloured icon chip mirroring the canvas card. -->
		<div class="flex size-9 shrink-0 items-center justify-center rounded-md {meta.chipClass}">
			<Icon class="size-5 {meta.iconClass}" />
		</div>

		<div class="min-w-0 flex-1">
			<h2 class="truncate text-base font-semibold text-foreground" data-testid="inspector-label">
				{label}
			</h2>
			<div class="mt-1 flex flex-wrap items-center gap-2 text-sm">
				<Badge variant="outline" class="font-mono">{meta.label}</Badge>
				{@render status?.()}
			</div>
			{#if nodeId}
				<div class="mt-1 truncate font-mono text-sm text-muted-foreground/80" title={nodeId}>
					id: {nodeId}
				</div>
			{/if}
			{#if description}
				<p class="mt-1 line-clamp-2 text-sm text-muted-foreground">{description}</p>
			{/if}
		</div>

		<div class="flex shrink-0 items-center gap-1">
			{@render actions?.()}
			{@render close?.()}
		</div>
	</header>

	<div class={bodyClass}>
		{@render children()}
	</div>
</div>
