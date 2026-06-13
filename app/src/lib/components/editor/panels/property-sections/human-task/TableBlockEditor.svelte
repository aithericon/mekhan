<script lang="ts">
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import { Input } from '$lib/components/ui/input';
	import { Textarea } from '$lib/components/ui/textarea';
	import { Label } from '$lib/components/ui/label';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import InsertRefButton from '../InsertRefButton.svelte';

	type Props = {
		headers: string[];
		rows_ref?: string;
		caption?: string;
		readonly?: boolean;
		scope?: ScopeEntry[];
		onchange: (updated: { headers: string[]; rows_ref?: string; caption?: string }) => void;
		onremove: () => void;
	};

	let {
		headers,
		rows_ref,
		caption,
		readonly = false,
		scope = [],
		onchange,
		onremove
	}: Props = $props();

	function emit(patch: Partial<{ headers: string[]; rows_ref?: string; caption?: string }>) {
		onchange({ headers, rows_ref, caption, ...patch });
	}

	// `{{ slug.field }}` ref-picker snippets carry interpolation braces;
	// rows_ref is a structured whole-array ref — strip them.
	function setRowsRef(snippet: string) {
		emit({ rows_ref: snippet.replace(/[{}]/g, '').trim() || undefined });
	}
</script>

<!-- ui-allow: block-type accent — no theme token for table/teal identity -->
<div class="rounded-md border border-border/50 border-l-2 border-l-teal-400 bg-background p-3">
	<div class="mb-2 flex items-center justify-between">
		<!-- ui-allow: block-type badge color — no theme token for table/teal identity -->
		<span
			class="rounded bg-teal-100 px-2 py-0.5 text-sm font-medium text-teal-700 dark:bg-teal-900/30 dark:text-teal-300"
			>Table</span
		>
		{#if !readonly}
			<button
				type="button"
				class="rounded p-1 text-muted-foreground transition-colors hover:text-destructive"
				onclick={onremove}
			>
				<Trash2 class="size-4" />
			</button>
		{/if}
	</div>
	<div class="space-y-2">
		<div>
			<Label class="mb-1 text-xs text-muted-foreground">Column headers (one per line)</Label>
			<Textarea
				value={headers.join('\n')}
				placeholder={'#\nName\nValue'}
				disabled={readonly}
				rows={3}
				class="font-mono"
				oninput={(e) =>
					emit({
						headers: (e.currentTarget as HTMLTextAreaElement).value
							.split('\n')
							.filter((h) => h.trim().length > 0)
					})}
			/>
		</div>
		<div>
			<Label class="mb-1 text-xs text-muted-foreground">
				Rows reference (upstream array of rows, e.g. <code>report.table_rows</code>)
			</Label>
			<Input
				value={rows_ref ?? ''}
				placeholder="slug.field"
				disabled={readonly}
				class="font-mono"
				oninput={(e) =>
					emit({ rows_ref: (e.currentTarget as HTMLInputElement).value.trim() || undefined })}
			/>
			{#if scope.length > 0}
				<div class="mt-1.5">
					<InsertRefButton {scope} disabled={readonly} oninsert={setRowsRef} />
				</div>
			{/if}
		</div>
		<div>
			<Label class="mb-1 text-xs text-muted-foreground">Caption (optional)</Label>
			<Input
				value={caption ?? ''}
				placeholder="Table caption…"
				disabled={readonly}
				oninput={(e) =>
					emit({ caption: (e.currentTarget as HTMLInputElement).value || undefined })}
			/>
		</div>
	</div>
</div>
