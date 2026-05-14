<script lang="ts">
	import Info from '@lucide/svelte/icons/info';
	import TriangleAlert from '@lucide/svelte/icons/triangle-alert';
	import CircleX from '@lucide/svelte/icons/circle-x';
	import CircleCheck from '@lucide/svelte/icons/circle-check';

	let { severity, title, content, contentHtml }: {
		severity: 'info' | 'warning' | 'error' | 'success';
		title?: string;
		content?: string;
		contentHtml?: string;
	} = $props();

	const config: Record<string, { border: string; bg: string; text: string }> = {
		info: { border: 'border-info/25', bg: 'bg-info/10', text: 'text-info' },
		warning: { border: 'border-warning/25', bg: 'bg-warning/10', text: 'text-warning' },
		error: { border: 'border-destructive/25', bg: 'bg-destructive/10', text: 'text-destructive' },
		success: { border: 'border-success/25', bg: 'bg-success/10', text: 'text-success' }
	};

	const current = $derived(config[severity] ?? config.info);

	/** Convert basic markdown inline formatting to HTML. */
	function inlineMarkdown(text: string): string {
		return text
			.replace(/&/g, '&amp;')
			.replace(/</g, '&lt;')
			.replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>')
			.replace(/\*(.+?)\*/g, '<em>$1</em>')
			.replace(/`(.+?)`/g, '<code>$1</code>')
			.replace(/\n/g, '<br>');
	}
</script>

<div class="flex gap-3 rounded-xl border {current.border} {current.bg} p-4">
	<div class="shrink-0 pt-0.5">
		{#if severity === 'info'}
			<Info class="size-5 {current.text}" />
		{:else if severity === 'warning'}
			<TriangleAlert class="size-5 {current.text}" />
		{:else if severity === 'error'}
			<CircleX class="size-5 {current.text}" />
		{:else}
			<CircleCheck class="size-5 {current.text}" />
		{/if}
	</div>
	<div class="min-w-0 flex-1">
		{#if title}
			<div class="mb-1 text-base font-semibold text-foreground">{title}</div>
		{/if}
		<div class="prose-base text-foreground/90">
			{#if contentHtml}
				<!-- eslint-disable-next-line svelte/no-at-html-tags -->
				{@html contentHtml}
			{:else if content}
				<!-- eslint-disable-next-line svelte/no-at-html-tags -->
				{@html inlineMarkdown(content)}
			{/if}
		</div>
	</div>
</div>
