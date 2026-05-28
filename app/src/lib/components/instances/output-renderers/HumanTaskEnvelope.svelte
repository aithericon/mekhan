<script lang="ts">
	import { Badge } from '$lib/components/ui/badge';
	import KeyValueList from './KeyValueList.svelte';
	import type { RendererProps } from './types';

	type Envelope = {
		task_id?: string;
		status?: string;
		completed_at?: string;
		data?: Record<string, unknown>;
		[k: string]: unknown;
	};

	let { value, ctx }: RendererProps = $props();
	const env = $derived(value as Envelope);

	const formData = $derived(env.data ?? {});
	const hasFormData = $derived(
		formData && typeof formData === 'object' && Object.keys(formData).length > 0
	);

	function formatTime(iso: string | undefined): string | null {
		if (!iso) return null;
		try {
			return new Date(iso).toLocaleString();
		} catch {
			return iso;
		}
	}
</script>

<div class="space-y-3">
	<div class="flex flex-wrap items-center gap-2 text-sm">
		{#if env.status}
			<Badge variant="secondary" class="font-mono">{env.status}</Badge>
		{/if}
		{#if env.task_id}
			<span class="text-muted-foreground">task</span>
			<code class="rounded bg-muted px-1.5 py-0.5 font-mono text-sm">{env.task_id}</code>
		{/if}
		{#if env.completed_at}
			<span class="text-muted-foreground">·</span>
			<span class="text-muted-foreground">completed {formatTime(env.completed_at)}</span>
		{/if}
	</div>

	{#if hasFormData}
		<div>
			<div class="mb-1.5 text-sm font-semibold text-foreground">Form submission</div>
			<KeyValueList value={formData} {ctx} />
		</div>
	{:else}
		<div class="text-sm text-muted-foreground italic">No form submission data.</div>
	{/if}
</div>
