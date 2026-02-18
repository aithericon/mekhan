<script lang="ts">
	import Upload from '@lucide/svelte/icons/upload';
	import LayoutGrid from '@lucide/svelte/icons/layout-grid';
	import type { Awareness } from 'y-protocols/awareness';
	import type { MekhanWsProvider } from '$lib/yjs/ws-provider';
	import AwarenessBar from '$lib/components/editor/AwarenessBar.svelte';
	import ConnectionStatus from '$lib/components/editor/ConnectionStatus.svelte';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';

	type Props = {
		templateName: string;
		templateId: string;
		published: boolean;
		awareness?: Awareness;
		provider?: MekhanWsProvider;
		onPublish: () => void;
	};

	let { templateName, templateId, published, awareness, provider, onPublish }: Props = $props();
</script>

<div class="flex h-10 items-center justify-between border-b border-border bg-card px-3">
	<div class="flex items-center gap-3">
		<span class="text-sm font-medium text-foreground">{templateName}</span>
		{#if published}
			<Badge class="bg-green-100 text-green-700" variant="secondary">
				Published
			</Badge>
		{:else}
			<Badge class="bg-amber-100 text-amber-700" variant="secondary">
				Draft
			</Badge>
		{/if}
		{#if provider}
			<ConnectionStatus {provider} />
		{/if}
		{#if awareness}
			<AwarenessBar {awareness} />
		{/if}
	</div>

	<div class="flex items-center gap-1.5">
		<Button variant="ghost" size="sm" href="/templates/{templateId}">
			<LayoutGrid class="size-3.5" />
			Canvas Mode
		</Button>

		<Button size="sm" disabled={published} onclick={onPublish}>
			<Upload class="size-3.5" />
			Publish
		</Button>
	</div>
</div>
