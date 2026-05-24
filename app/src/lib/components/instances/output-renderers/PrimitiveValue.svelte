<script lang="ts">
	import { Badge } from '$lib/components/ui/badge';
	import type { RendererProps } from './types';

	let { value }: RendererProps = $props();

	const isString = $derived(typeof value === 'string');
	const isMultiline = $derived(isString && (value as string).includes('\n'));
	const isNull = $derived(value === null || value === undefined);
	const isBool = $derived(typeof value === 'boolean');
	const isNumber = $derived(typeof value === 'number');
</script>

{#if isNull}
	<span class="text-sm italic text-muted-foreground">null</span>
{:else if isBool}
	<Badge variant={value ? 'default' : 'secondary'} class="font-mono">
		{value ? 'true' : 'false'}
	</Badge>
{:else if isNumber}
	<span class="font-mono text-sm text-foreground">{value}</span>
{:else if isMultiline}
	<!-- Treat multi-line strings as content (LLM responses, doc text, code) —
	     render with whitespace preserved but no JSON quote/escape noise. -->
	<div class="rounded-md border border-border bg-muted/30 p-3 text-sm whitespace-pre-wrap break-words">{value}</div>
{:else if isString}
	<span class="text-sm text-foreground break-words">{value}</span>
{/if}
