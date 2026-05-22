<script lang="ts">
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import { Textarea } from '$lib/components/ui/textarea';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import InsertRefButton from '../InsertRefButton.svelte';

	type Props = {
		content: string;
		readonly?: boolean;
		scope?: ScopeEntry[];
		onchange: (content: string) => void;
		onremove: () => void;
	};

	let {
		content,
		readonly = false,
		scope = [],
		onchange,
		onremove
	}: Props = $props();

	function appendRef(snippet: string) {
		onchange(content ? `${content} ${snippet}` : snippet);
	}
</script>

<!-- ui-allow: block-type accent — no theme token for markdown/purple identity -->
<div class="rounded-md border border-border/50 border-l-2 border-l-purple-400 bg-background p-3">
	<div class="mb-2 flex items-center justify-between">
		<!-- ui-allow: block-type badge color — no theme token for markdown/purple identity -->
		<span
			class="rounded bg-purple-100 px-2 py-0.5 text-sm font-medium text-purple-700 dark:bg-purple-900/30 dark:text-purple-300"
			>Markdown</span
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
	<Textarea
		value={content}
		placeholder="Markdown content..."
		disabled={readonly}
		oninput={(e) => onchange((e.currentTarget as HTMLTextAreaElement).value)}
		rows={4}
		class="font-mono"
	/>
	{#if scope.length > 0}
		<div class="mt-1.5">
			<InsertRefButton {scope} disabled={readonly} oninsert={appendRef} />
		</div>
	{/if}
</div>
