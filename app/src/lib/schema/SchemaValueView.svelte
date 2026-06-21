<script lang="ts">
	/**
	 * Schema-aware expandable value tree.
	 *
	 * Renders an unknown runtime value as a collapsible tree, annotating each
	 * key with its declared type label from `ty` when available. Delegates
	 * leaves to the existing renderers (PrimitiveValue, FileReference,
	 * StorageRefValue). Nested objects expand inline instead of collapsing to
	 * compactJson.
	 *
	 * Performance: collapsed branches render nothing below the toggle row —
	 * effectively lazy. Top 2 levels auto-expand; deeper levels start collapsed.
	 */
	import type { TyDescriptor } from '$lib/editor/guard-scope';
	import type { RenderContext } from '$lib/components/instances/output-renderers/types';
	import { tyDescriptorToSchemaNode, isPrimitive, isFileRef, isStorageKey } from './model';
	import type { SchemaNode } from './model';
	import PrimitiveValue from '$lib/components/instances/output-renderers/PrimitiveValue.svelte';
	import FileReference from '$lib/components/instances/output-renderers/FileReference.svelte';
	import StorageRefValue from '$lib/components/instances/output-renderers/StorageRefValue.svelte';
	// Self-import for recursion (replaces the deprecated <svelte:self>).
	import SchemaValueView from './SchemaValueView.svelte';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';

	type Props = {
		value: unknown;
		ty?: TyDescriptor;
		/** Pre-built schema node. Takes precedence over `ty` — lets callers that
		 *  already hold a `SchemaNode` (e.g. catalogue file-metadata columns) drive
		 *  the tree without a round-trip through `TyDescriptor`. */
		schemaNode?: SchemaNode;
		ctx: RenderContext;
		/** Internal: current nesting depth (callers leave this absent). */
		depth?: number;
	};

	let { value, ty, schemaNode, ctx, depth = 0 }: Props = $props();

	const schema = $derived(schemaNode ?? tyDescriptorToSchemaNode(ty));

	// Element schema for arrays — propagated to each item so nested object keys
	// inside array elements stay type-annotated, not just the top level.
	const elementSchema = $derived<SchemaNode | null>(
		schema.kind === 'array' ? schema.element : null
	);

	// Auto-expand the top 2 levels; deeper levels start collapsed.
	const AUTO_EXPAND_DEPTH = 2;

	// ── Object rendering ──────────────────────────────────────────────────────

	type ObjEntry = {
		key: string;
		val: unknown;
		childSchema: SchemaNode | null;
	};

	const isObj = $derived(
		value !== null && value !== undefined && typeof value === 'object' && !Array.isArray(value)
	);

	const objEntries = $derived.by<ObjEntry[]>(() => {
		if (!isObj || typeof value !== 'object' || value === null || Array.isArray(value)) return [];
		const rec = value as Record<string, unknown>;
		return Object.entries(rec).map(([key, val]) => {
			let childSchema: SchemaNode | null = null;
			if (schema.kind === 'object') {
				childSchema = schema.fields.get(key) ?? null;
			}
			return { key, val, childSchema };
		});
	});

	// ── Array rendering ───────────────────────────────────────────────────────

	const isArr = $derived(Array.isArray(value));

	const arrItems = $derived.by<{ idx: number; val: unknown }[]>(() => {
		if (!isArr || !Array.isArray(value)) return [];
		return (value as unknown[]).map((val, idx) => ({ idx, val }));
	});

	// ── Expand/collapse per-key state ─────────────────────────────────────────

	let expanded = $state<Set<string>>(new Set());

	// Seed auto-expansion of the top levels on mount/value change.
	$effect(() => {
		// Reactive on value/depth — recalculate initial open set.
		void value;
		const next = new Set<string>();
		if (depth < AUTO_EXPAND_DEPTH) {
			if (isObj && typeof value === 'object' && value !== null && !Array.isArray(value)) {
				for (const k of Object.keys(value as Record<string, unknown>)) {
					next.add(`obj:${k}`);
				}
			} else if (isArr && Array.isArray(value)) {
				for (let i = 0; i < (value as unknown[]).length; i++) {
					next.add(`arr:${i}`);
				}
			}
		}
		expanded = next;
	});

	function toggle(key: string) {
		const next = new Set(expanded);
		if (next.has(key)) next.delete(key);
		else next.add(key);
		expanded = next;
	}

	/** Whether a value is complex enough to warrant expand/collapse. */
	function isExpandable(v: unknown): boolean {
		if (v === null || v === undefined) return false;
		if (Array.isArray(v)) return v.length > 0;
		if (typeof v === 'object') return Object.keys(v as object).length > 0;
		return false;
	}

	function compactJson(v: unknown): string {
		try {
			const s = JSON.stringify(v);
			return s.length > 60 ? s.slice(0, 57) + '…' : s;
		} catch {
			return String(v);
		}
	}
</script>

{#if isFileRef(value)}
	<FileReference {value} {ctx} />
{:else if isStorageKey(value)}
	<StorageRefValue {value} {ctx} />
{:else if isPrimitive(value)}
	<PrimitiveValue {value} {ctx} />
{:else if isObj}
	<!-- Object: expandable key rows, or an empty-object placeholder -->
	{#if objEntries.length === 0}
		<span class="text-sm italic text-muted-foreground">{'{}'}</span>
	{/if}
	<dl class="space-y-0.5">
		{#each objEntries as entry (entry.key)}
			{@const rowKey = `obj:${entry.key}`}
			{@const canExpand = isExpandable(entry.val)}
			{@const open = expanded.has(rowKey)}
			<div class="min-w-0">
				<div class="flex items-start gap-1">
					<!-- Expand toggle or indent placeholder -->
					<div class="mt-0.5 shrink-0">
						{#if canExpand}
							<button
								type="button"
								class="-mx-0.5 inline-flex size-4 items-center justify-center rounded text-muted-foreground hover:bg-muted hover:text-foreground"
								onclick={() => toggle(rowKey)}
								aria-label={open ? 'Collapse' : 'Expand'}
							>
								{#if open}
									<ChevronDown class="size-3" />
								{:else}
									<ChevronRight class="size-3" />
								{/if}
							</button>
						{:else}
							<span class="inline-block size-4"></span>
						{/if}
					</div>

					<!-- Key + type badge -->
					<dt class="shrink-0 font-mono text-sm text-muted-foreground" title={entry.key}>
						{entry.key}
					</dt>
					{#if entry.childSchema}
						<span class="shrink-0 text-xs text-muted-foreground/60">
							: {entry.childSchema.label}
						</span>
					{/if}

					<!-- Inline value for non-expandable leaves -->
					{#if !canExpand}
						<dd class="min-w-0 break-words">
							<SchemaValueView
									value={entry.val}
									schemaNode={entry.childSchema ?? undefined}
									{ctx}
									depth={depth + 1}
								/>
						</dd>
					{:else if !open}
						<!-- Collapsed preview: use a button for accessibility -->
						<dd class="min-w-0">
							<button
								type="button"
								class="rounded bg-muted px-1 py-0.5 font-mono text-xs text-muted-foreground hover:bg-muted/80"
								onclick={() => toggle(rowKey)}
								title="Click to expand"
							>
								{compactJson(entry.val)}
							</button>
						</dd>
					{/if}
				</div>

				<!-- Expanded nested value -->
				{#if canExpand && open}
					<dd class="ml-5 mt-1 border-l border-border/50 pl-3">
						<SchemaValueView
									value={entry.val}
									schemaNode={entry.childSchema ?? undefined}
									{ctx}
									depth={depth + 1}
								/>
					</dd>
				{/if}
			</div>
		{/each}
	</dl>
{:else if isArr}
	<!-- Array: expandable index rows -->
	{#if arrItems.length === 0}
		<span class="text-sm italic text-muted-foreground">[]</span>
	{:else}
		<div class="space-y-0.5">
			{#each arrItems as item (item.idx)}
				{@const rowKey = `arr:${item.idx}`}
				{@const canExpand = isExpandable(item.val)}
				{@const open = expanded.has(rowKey)}
				<div class="min-w-0">
					<div class="flex items-start gap-1">
						<div class="mt-0.5 shrink-0">
							{#if canExpand}
								<button
									type="button"
									class="-mx-0.5 inline-flex size-4 items-center justify-center rounded text-muted-foreground hover:bg-muted hover:text-foreground"
									onclick={() => toggle(rowKey)}
									aria-label={open ? 'Collapse' : 'Expand'}
								>
									{#if open}
										<ChevronDown class="size-3" />
									{:else}
										<ChevronRight class="size-3" />
									{/if}
								</button>
							{:else}
								<span class="inline-block size-4"></span>
							{/if}
						</div>
						<span class="shrink-0 font-mono text-xs text-muted-foreground/60">[{item.idx}]</span>
						{#if !canExpand}
							<div class="min-w-0 break-words">
								<SchemaValueView
									value={item.val}
									schemaNode={elementSchema ?? undefined}
									{ctx}
									depth={depth + 1}
								/>
							</div>
						{:else if !open}
							<button
								type="button"
								class="min-w-0 rounded bg-muted px-1 py-0.5 font-mono text-xs text-muted-foreground hover:bg-muted/80"
								onclick={() => toggle(rowKey)}
								title="Click to expand"
							>
								{compactJson(item.val)}
							</button>
						{/if}
					</div>
					{#if canExpand && open}
						<div class="ml-5 mt-1 border-l border-border/50 pl-3">
							<SchemaValueView
									value={item.val}
									schemaNode={elementSchema ?? undefined}
									{ctx}
									depth={depth + 1}
								/>
						</div>
					{/if}
				</div>
			{/each}
		</div>
	{/if}
{/if}
