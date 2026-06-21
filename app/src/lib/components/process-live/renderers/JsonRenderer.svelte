<script lang="ts">
	import { artifactFetchUrl } from '$lib/api/client';
	import { authFetch } from '$lib/auth/fetch';
	import type { LiveArtifactEntry } from '$lib/api/client';
	import SchemaValueView from '$lib/schema/SchemaValueView.svelte';
	import CodeEditor from '$lib/components/editor/panels/shared/CodeEditor.svelte';
	import CopyButton from '$lib/components/ui/copy-button/CopyButton.svelte';
	import type { RenderContext } from '$lib/components/instances/output-renderers/types';
	import type { SchemaNode } from '$lib/schema/model';

	interface Props {
		entry: LiveArtifactEntry;
		/** Per-record schema for the file's content (catalogue file-metadata
		 *  columns). Annotates the tree with field types. When the parsed value is
		 *  an array, this is treated as the element schema. */
		schemaNode?: SchemaNode;
	}
	let { entry, schemaNode }: Props = $props();

	let raw = $state<string | null>(null);
	let parsed = $state<unknown>(undefined);
	let parseOk = $state(false);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let view = $state<'tree' | 'raw'>('tree');

	// Catalogue previews carry no producer schema to annotate against, so the
	// tree renders untyped — but it's the same interactive component the instance
	// output views use, not a second inferior reimplementation.
	const ctx: RenderContext = { position: 'output' };

	// A top-level object/array is worth the expandable tree; a scalar/string
	// document (or unparseable text) only has a raw view.
	const canTree = $derived(parseOk && typeof parsed === 'object' && parsed !== null);

	// The record schema describes one object; an array-of-objects file wraps it.
	const effectiveSchema = $derived<SchemaNode | undefined>(
		!schemaNode
			? undefined
			: Array.isArray(parsed)
				? { kind: 'array', element: schemaNode, label: `array<${schemaNode.label}>` }
				: schemaNode
	);

	$effect(() => {
		const id = entry.artifact_id ?? entry.id;
		void id;
		raw = null;
		parsed = undefined;
		parseOk = false;
		loading = true;
		error = null;
		const url = artifactFetchUrl(entry);
		if (!url) {
			loading = false;
			error = 'no content url';
			return;
		}
		const controller = new AbortController();
		authFetch(url, { signal: controller.signal })
			.then((r) => {
				if (!r.ok) throw new Error(`fetch failed: ${r.status}`);
				return r.text();
			})
			.then((t) => {
				try {
					const v = JSON.parse(t);
					parsed = v;
					parseOk = true;
					raw = JSON.stringify(v, null, 2);
					view = typeof v === 'object' && v !== null ? 'tree' : 'raw';
				} catch {
					raw = t;
					parseOk = false;
					view = 'raw';
				}
				loading = false;
			})
			.catch((e) => {
				if (controller.signal.aborted) return;
				error = e instanceof Error ? e.message : String(e);
				loading = false;
			});
		return () => controller.abort();
	});
</script>

<div class="flex flex-col gap-2">
	{#if loading}
		<div class="text-sm text-muted-foreground">Loading JSON…</div>
	{:else if error}
		<div class="text-sm text-red-500">{error}</div>
	{:else}
		<div class="flex items-center justify-between gap-2">
			{#if canTree}
				<div class="inline-flex overflow-hidden rounded-md border border-border text-xs">
					<button
						type="button"
						class={`px-2 py-1 ${view === 'tree' ? 'bg-muted text-foreground' : 'text-muted-foreground hover:bg-muted/50'}`}
						onclick={() => (view = 'tree')}
					>
						Tree
					</button>
					<button
						type="button"
						class={`border-l border-border px-2 py-1 ${view === 'raw' ? 'bg-muted text-foreground' : 'text-muted-foreground hover:bg-muted/50'}`}
						onclick={() => (view = 'raw')}
					>
						Raw
					</button>
				</div>
			{:else}
				<span></span>
			{/if}
			{#if raw !== null}
				<CopyButton text={raw} title="Copy JSON" class="size-7 justify-center" />
			{/if}
		</div>

		{#if view === 'tree' && canTree}
			<div class="max-h-[60vh] overflow-auto rounded-lg border border-border bg-card p-3">
				<SchemaValueView value={parsed} schemaNode={effectiveSchema} {ctx} />
			</div>
		{:else if raw !== null}
			<CodeEditor
				value={raw}
				language="json"
				readonly
				dimWhenReadonly={false}
				minHeight="80px"
				maxHeight="60vh"
			/>
		{/if}
	{/if}
	<p class="truncate text-sm text-muted-foreground">
		{entry.filename}
		{#if entry.size_bytes}
			· {(entry.size_bytes / 1024).toFixed(1)} KB
		{/if}
	</p>
</div>
