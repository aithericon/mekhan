<script lang="ts">
	import { Button } from '$lib/components/ui/button';
	import Braces from '@lucide/svelte/icons/braces';
	import { pickRenderer, FALLBACK } from './index';
	import type { RenderContext } from './types';

	type Props = {
		value: unknown;
		ctx: RenderContext;
	};

	let { value, ctx }: Props = $props();

	const picked = $derived(pickRenderer(value, ctx));
	let raw = $state(false);

	const Renderer = $derived(raw ? FALLBACK.component : picked.component);
	const showToggle = $derived(picked.name !== 'json');
</script>

<div class="space-y-1.5">
	<Renderer {value} {ctx} />
	{#if showToggle}
		<div class="flex justify-end">
			<Button
				variant="ghost"
				size="sm"
				class="h-auto px-2 py-0.5 text-sm text-muted-foreground hover:text-foreground"
				onclick={() => (raw = !raw)}
				title={raw ? `Switch back to ${picked.label} view` : 'View raw JSON'}
			>
				<Braces class="size-3.5" />
				<span class="ml-1">{raw ? picked.label : 'JSON'}</span>
			</Button>
		</div>
	{/if}
</div>
