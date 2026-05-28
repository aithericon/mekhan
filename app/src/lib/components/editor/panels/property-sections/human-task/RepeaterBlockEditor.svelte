<script lang="ts">
	// Feature B — Repeater block config UI.
	//
	// Authors point `items_ref` at an upstream array (RefPicker with
	// `allowArrayBoundary=true` so the synthetic `[*]` row is offered),
	// optionally pick a per-element label, declare a sub-task body
	// (any block type except a nested Repeater) that renders per row,
	// and assign a Rhai-safe `output_slug` for the typed array output
	// downstream. The element schema of `<output_slug>.results` is
	// derived server-side from the Input child blocks only — display
	// children render but contribute nothing to the typed shape.
	import type { TaskBlockConfig } from '$lib/types/editor';
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import { Input } from '$lib/components/ui/input';
	import { Label } from '$lib/components/ui/label';
	import RefPicker from '../RefPicker.svelte';
	import BlockListEditor from './BlockListEditor.svelte';

	type Props = {
		items_ref: string;
		item_label_ref?: string;
		blocks: TaskBlockConfig[];
		output_slug: string;
		readonly?: boolean;
		binding?: YjsGraphBinding;
		nodeId?: string;
		scope?: ScopeEntry[];
		onchange: (next: {
			items_ref: string;
			item_label_ref: string | undefined;
			blocks: TaskBlockConfig[];
			output_slug: string;
		}) => void;
		onremove: () => void;
	};

	let {
		items_ref,
		item_label_ref,
		blocks,
		output_slug,
		readonly = false,
		binding,
		nodeId,
		scope = [],
		onchange,
		onremove
	}: Props = $props();

	let expanded = $state(true);

	function emit(patch: Partial<Props>) {
		onchange({
			items_ref: patch.items_ref ?? items_ref,
			item_label_ref: patch.item_label_ref ?? item_label_ref,
			blocks: patch.blocks ?? blocks,
			output_slug: patch.output_slug ?? output_slug
		});
	}
</script>

<div class="rounded-md border border-border/50 bg-background text-sm">
	<!-- Header -->
	<div class="flex items-center gap-2 p-2.5">
		<button
			type="button"
			class="rounded p-0.5 text-muted-foreground transition-colors hover:text-foreground"
			onclick={() => (expanded = !expanded)}
			aria-label="Toggle Repeater section"
		>
			{#if expanded}
				<ChevronDown class="size-4" />
			{:else}
				<ChevronRight class="size-4" />
			{/if}
		</button>
		<!-- ui-allow: block-type swatch — repeater identity uses violet -->
		<span class="size-2.5 rounded-sm bg-violet-400"></span>
		<span class="text-sm font-medium text-foreground">Repeater</span>
		<span class="ml-1 truncate text-sm text-muted-foreground">
			{items_ref || '— pick an upstream array —'}
		</span>
		<div class="ml-auto flex items-center gap-1">
			{#if !readonly}
				<button
					type="button"
					class="rounded p-1 text-muted-foreground transition-colors hover:text-destructive"
					onclick={onremove}
					aria-label="Remove Repeater block"
				>
					<Trash2 class="size-4" />
				</button>
			{/if}
		</div>
	</div>

	{#if expanded}
		<div class="space-y-3 border-t border-border/50 p-3">
			<!-- items_ref picker -->
			<div class="space-y-1.5">
				<Label class="text-sm text-muted-foreground">Items source (upstream array)</Label>
				<RefPicker
					{scope}
					selected={items_ref}
					placeholder="Pick an array field…"
					allowArrayBoundary={true}
					disabled={readonly}
					onpick={(entry) => emit({ items_ref: entry.qualified })}
				/>
				<p class="text-sm text-muted-foreground">
					Authors pick a <code>[*]</code> array boundary like
					<code>extract.tasks[*]</code>; one sub-form row renders per element.
				</p>
			</div>

			<!-- item_label_ref picker -->
			<div class="space-y-1.5">
				<Label class="text-sm text-muted-foreground">Row label (optional)</Label>
				<div class="flex items-center gap-2">
					<div class="flex-1">
						<RefPicker
							{scope}
							selected={item_label_ref}
							placeholder="Pick a per-element label…"
							allowArrayBoundary={true}
							disabled={readonly}
							onpick={(entry) => emit({ item_label_ref: entry.qualified })}
						/>
					</div>
					{#if item_label_ref && !readonly}
						<button
							type="button"
							class="rounded p-1 text-muted-foreground transition-colors hover:text-destructive"
							onclick={() => emit({ item_label_ref: undefined })}
							aria-label="Clear row label"
						>
							<Trash2 class="size-4" />
						</button>
					{/if}
				</div>
				<p class="text-sm text-muted-foreground">
					Must share the iteration prefix of the items source. Defaults to <code>Item N</code>.
				</p>
			</div>

			<!-- output_slug -->
			<div class="space-y-1.5">
				<Label class="text-sm text-muted-foreground">Output slug</Label>
				<Input
					type="text"
					value={output_slug}
					placeholder="review_tasks"
					disabled={readonly}
					oninput={(e) =>
						emit({ output_slug: (e.currentTarget as HTMLInputElement).value })}
					class="font-mono"
				/>
				<p class="text-sm text-muted-foreground">
					Rhai-safe identifier; downstream picks the typed array via
					<code>{output_slug || '<output_slug>'}[*].&lt;sub_field&gt;</code>.
				</p>
			</div>

			<!-- Per-row body — any block type EXCEPT a nested Repeater. -->
			<div class="space-y-1.5">
				<Label class="text-sm text-muted-foreground">Per-row body</Label>
				{#if blocks.length === 0}
					<div class="rounded-md border border-dashed border-border bg-muted/30 px-3 py-2 text-sm text-muted-foreground">
						No blocks yet. Add one — every block renders once per upstream
						element. Input blocks become the typed per-row output schema.
					</div>
				{/if}
				<BlockListEditor
					{blocks}
					{readonly}
					{binding}
					{nodeId}
					{scope}
					excludeRepeater={true}
					onchange={(next) => emit({ blocks: next })}
				/>
			</div>
		</div>
	{/if}
</div>
