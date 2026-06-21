<script lang="ts">
	/**
	 * Read-only recursive schema/type tree. Renders a `SchemaNode` as an
	 * expand/collapse tree with type-label badges.
	 *
	 * Modelled on `RefPicker`'s `walkTy` recursion but read-only — no pick
	 * action. Suitable for node I/O-contract displays and sub-workflow previews.
	 *
	 * Props accept a `SchemaNode` directly (the canonical model). Callers that
	 * have a raw `TyDescriptor` or `Port` should convert via
	 * `tyDescriptorToSchemaNode` / `portToSchemaNode` from `./model` before
	 * passing in.
	 */
	import { untrack } from 'svelte';
	import type { Snippet } from 'svelte';
	import type { SchemaNode } from './model';
	// Self-import for recursion (replaces the deprecated <svelte:self>).
	import SchemaView from './SchemaView.svelte';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';

	type Props = {
		node: SchemaNode;
		/** Optional field label; shown as the field name when present. */
		label?: string;
		/** Optional trailing content for THIS node's header row (not propagated to
		 *  children) — e.g. catalogue nullable / classification chips that belong
		 *  on the field line rather than in a separate table column. */
		trailing?: Snippet;
		/** Internal — callers leave this absent. */
		depth?: number;
	};

	let { node, label, trailing, depth = 0 }: Props = $props();

	// depth is a stable, mount-time prop — auto-expand the first two levels.
	// untrack() makes the $state initializer explicitly snapshot-only so
	// Svelte knows this is intentional and not a reactive read oversight.
	let expanded = $state(untrack(() => depth < 2));

	// Only show the expand chevron when there are children worth exploring.
	// Array<scalar> does NOT get a chevron: the element type is shown inline in
	// the badge, and expanding to a "[*] String" row adds no information.
	const hasChildren = $derived(
		node.kind === 'object'
			? node.fields.size > 0
			: node.kind === 'array'
				? node.element.kind === 'object' || node.element.kind === 'array'
				: false
	);

	// Convenience: field count for the collapsed object hint.
	const fieldCount = $derived(node.kind === 'object' ? node.fields.size : 0);

	// Inline element label for array badges (e.g. "<String>").
	const arrayElemLabel = $derived(node.kind === 'array' ? node.element.label : null);

	// For array<scalar>: which scalar name to colour-badge inline.
	const arrayElemScalarName = $derived(
		node.kind === 'array' && node.element.kind === 'scalar' ? node.element.name : null
	);

	/** Badge background colour by scalar name. */
	function scalarBadgeClass(name: string): string {
		switch (name) {
			case 'String':
				return 'bg-blue-100 text-blue-700 dark:bg-blue-900/30 dark:text-blue-300';
			case 'Number':
				return 'bg-purple-100 text-purple-700 dark:bg-purple-900/30 dark:text-purple-300';
			case 'Bool':
				return 'bg-amber-100 text-amber-700 dark:bg-amber-900/30 dark:text-amber-300';
			case 'FileRef':
				return 'bg-green-100 text-green-700 dark:bg-green-900/30 dark:text-green-300';
			case 'Timestamp':
				return 'bg-cyan-100 text-cyan-700 dark:bg-cyan-900/30 dark:text-cyan-300';
			default:
				return 'bg-muted text-muted-foreground';
		}
	}

	function kindBadgeClass(k: string): string {
		switch (k) {
			case 'object':
				return 'bg-slate-100 text-slate-700 dark:bg-slate-800 dark:text-slate-300';
			case 'array':
				return 'bg-indigo-100 text-indigo-700 dark:bg-indigo-900/30 dark:text-indigo-300';
			default:
				return 'bg-muted text-muted-foreground';
		}
	}
</script>

<div class="min-w-0">
	<div class="flex items-center gap-1.5" style:padding-left="{depth * 14}px">
		<!-- Expand toggle -->
		<div class="shrink-0">
			{#if hasChildren}
				<button
					type="button"
					class="-mx-0.5 inline-flex size-4 items-center justify-center rounded text-muted-foreground hover:bg-muted hover:text-foreground"
					onclick={() => (expanded = !expanded)}
					aria-label={expanded ? 'Collapse' : 'Expand'}
					aria-expanded={expanded}
				>
					{#if expanded}
						<ChevronDown class="size-3" />
					{:else}
						<ChevronRight class="size-3" />
					{/if}
				</button>
			{:else}
				<span class="inline-block size-4"></span>
			{/if}
		</div>

		<!-- Field label -->
		{#if label}
			<span class="font-mono text-sm text-foreground">{label}</span>
		{/if}

		<!-- Type badge -->
		{#if node.kind === 'scalar'}
			<span class="rounded px-1.5 py-0.5 font-mono text-xs {scalarBadgeClass(node.name)}">
				{node.name}
			</span>
		{:else if node.kind === 'array'}
			<span class="rounded px-1.5 py-0.5 font-mono text-xs {kindBadgeClass('array')}">
				array
			</span>
			{#if arrayElemScalarName !== null}
				<span class="text-xs text-muted-foreground">
					&lt;<span
						class="rounded px-1 py-0.5 font-mono text-xs {scalarBadgeClass(arrayElemScalarName)}"
					>{arrayElemScalarName}</span>&gt;
				</span>
			{:else if arrayElemLabel !== null}
				<span class="text-xs text-muted-foreground">&lt;{arrayElemLabel}&gt;</span>
			{/if}
		{:else if node.kind === 'object'}
			<span class="rounded px-1.5 py-0.5 font-mono text-xs {kindBadgeClass('object')}">
				object
			</span>
			{#if !expanded && fieldCount > 0}
				<span class="text-xs text-muted-foreground/60">
					{fieldCount} field{fieldCount === 1 ? '' : 's'}
				</span>
			{/if}
		{:else if node.kind === 'any'}
			<span class="rounded px-1.5 py-0.5 font-mono text-xs bg-muted text-muted-foreground">
				any
			</span>
		{:else if node.kind === 'opaque'}
			<span class="rounded bg-muted px-1.5 py-0.5 font-mono text-xs text-muted-foreground">
				{node.name}
			</span>
		{/if}

		{@render trailing?.()}
	</div>

	<!-- Children -->
	{#if hasChildren && expanded}
		{#if node.kind === 'object'}
			<div class="mt-0.5 space-y-0.5">
				{#each [...node.fields.entries()] as [fieldName, childNode] (fieldName)}
					<SchemaView node={childNode} label={fieldName} depth={depth + 1} />
				{/each}
			</div>
		{:else if node.kind === 'array'}
			<!-- Show the [*] element boundary then recurse into the complex element -->
			<div class="mt-0.5" style:padding-left="{(depth + 1) * 14}px">
				<div class="flex items-center gap-1.5">
					<span class="inline-block size-4 shrink-0"></span>
					<span class="font-mono text-xs text-muted-foreground">[*]</span>
				</div>
				<SchemaView node={node.element} depth={depth + 2} />
			</div>
		{/if}
	{/if}
</div>
