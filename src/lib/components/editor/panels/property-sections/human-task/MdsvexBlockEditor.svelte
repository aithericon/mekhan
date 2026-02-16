<script lang="ts">
	import Trash2 from '@lucide/svelte/icons/trash-2';

	type Props = {
		content: string;
		readonly?: boolean;
		onchange: (content: string) => void;
		onremove: () => void;
	};

	let { content, readonly = false, onchange, onremove }: Props = $props();
</script>

<div class="rounded border border-border/50 border-l-2 border-l-purple-400 bg-background p-1.5">
	<div class="mb-1 flex items-center justify-between">
		<span
			class="rounded bg-purple-100 px-1.5 py-0.5 text-[9px] font-medium text-purple-700 dark:bg-purple-900/30 dark:text-purple-300"
			>Markdown</span
		>
		{#if !readonly}
			<button
				type="button"
				class="rounded p-0.5 text-muted-foreground transition-colors hover:text-destructive"
				onclick={onremove}
			>
				<Trash2 class="size-3" />
			</button>
		{/if}
	</div>
	<textarea
		value={content}
		placeholder="Markdown content..."
		disabled={readonly}
		oninput={(e) => onchange((e.currentTarget as HTMLTextAreaElement).value)}
		rows={3}
		class="w-full rounded border border-input bg-background px-1.5 py-1 font-mono text-[10px] text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
	></textarea>
</div>
