<script lang="ts">
	import Upload from '@lucide/svelte/icons/upload';
	import LayoutGrid from '@lucide/svelte/icons/layout-grid';
	import type { Awareness } from 'y-protocols/awareness';
	import type { MekhanWsProvider } from '$lib/yjs/ws-provider';
	import AwarenessBar from '$lib/components/editor/AwarenessBar.svelte';
	import ConnectionStatus from '$lib/components/editor/ConnectionStatus.svelte';

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
			<span class="rounded-full bg-green-100 px-2 py-0.5 text-[10px] font-medium text-green-700">
				Published
			</span>
		{:else}
			<span class="rounded-full bg-amber-100 px-2 py-0.5 text-[10px] font-medium text-amber-700">
				Draft
			</span>
		{/if}
		{#if provider}
			<ConnectionStatus {provider} />
		{/if}
		{#if awareness}
			<AwarenessBar {awareness} />
		{/if}
	</div>

	<div class="flex items-center gap-1.5">
		<a
			href="/templates/{templateId}"
			class="flex items-center gap-1.5 rounded-md px-2.5 py-1 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
		>
			<LayoutGrid class="size-3.5" />
			Canvas Mode
		</a>

		<button
			type="button"
			class="flex items-center gap-1.5 rounded-md bg-primary px-2.5 py-1 text-xs text-primary-foreground transition-colors hover:bg-primary/90 disabled:opacity-50"
			disabled={published}
			onclick={onPublish}
		>
			<Upload class="size-3.5" />
			Publish
		</button>
	</div>
</div>
