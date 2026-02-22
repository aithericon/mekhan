<script lang="ts">
	import Save from '@lucide/svelte/icons/save';
	import Upload from '@lucide/svelte/icons/upload';
	import Eye from '@lucide/svelte/icons/eye';
	import Code from '@lucide/svelte/icons/code';
	import type { Awareness } from 'y-protocols/awareness';
	import type { MekhanWsProvider } from '$lib/yjs/ws-provider';
	import AwarenessBar from '../AwarenessBar.svelte';
	import ConnectionStatus from '../ConnectionStatus.svelte';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';

	type Props = {
		templateName: string;
		published: boolean;
		saving: boolean;
		templateId?: string;
		awareness?: Awareness;
		provider?: MekhanWsProvider;
		onsave?: () => void;
		onpublish: () => void;
		onpreview: () => void;
	};

	let { templateName, published, saving, templateId, awareness, provider, onsave, onpublish, onpreview }: Props = $props();
</script>

<div class="flex h-10 items-center justify-between border-b border-border bg-card px-3" data-testid="editor-toolbar">
	<div class="flex items-center gap-3">
		<span class="text-sm font-medium text-foreground" data-testid="toolbar-template-name">{templateName}</span>
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
		{#if templateId}
			<Button
				variant="ghost"
				size="sm"
				href="/templates/{templateId}/ide"
				data-testid="btn-ide-mode"
			>
				<Code class="size-3.5" />
				IDE Mode
			</Button>
		{/if}

		<Button
			variant="ghost"
			size="sm"
			data-testid="btn-preview-air"
			onclick={onpreview}
		>
			<Eye class="size-3.5" />
			Preview AIR
		</Button>

		{#if onsave}
			<Button
				variant="ghost"
				size="sm"
				data-testid="btn-save"
				disabled={saving || published}
				onclick={onsave}
			>
				<Save class="size-3.5" />
				{saving ? 'Saving...' : 'Save'}
			</Button>
		{/if}

		<Button
			size="sm"
			data-testid="btn-publish"
			disabled={published}
			onclick={onpublish}
		>
			<Upload class="size-3.5" />
			Publish
		</Button>
	</div>
</div>
