<script lang="ts">
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import { Textarea } from '$lib/components/ui/textarea';

	type Props = {
		content: string;
		readonly?: boolean;
		onchange: (content: string) => void;
		onremove: () => void;
	};

	let { content, readonly = false, onchange, onremove }: Props = $props();
</script>

<div class="rounded-md border border-border/50 border-l-2 border-l-purple-400 bg-background p-3">
	<div class="mb-2 flex items-center justify-between">
		<span
			class="rounded bg-purple-100 px-2 py-0.5 text-xs font-medium text-purple-700 dark:bg-purple-900/30 dark:text-purple-300"
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
</div>
