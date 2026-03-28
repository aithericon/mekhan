<script lang="ts">
	import { X } from '@lucide/svelte';
	import MonacoEditor from './MonacoEditor.svelte';
	import { CopyButton } from '$lib/components/ui/copy-button';
	import type { Token } from '$lib/types/petri';

	interface Props {
		token: Token | null;
		placeName: string;
		open: boolean;
		onClose: () => void;
	}

	let { token, placeName, open, onClose }: Props = $props();

	const colorType = $derived(token?.color.type ?? 'Unit');
	const colorValue = $derived((token?.color as any)?.value);

	const typeBadgeClass = $derived.by(() => {
		switch (colorType) {
			case 'Unit':
				return 'bg-gray-500/15 text-gray-700 dark:text-gray-400';
			case 'Integer':
				return 'bg-violet-500/15 text-violet-700 dark:text-violet-400';
			case 'Data':
				return 'bg-pink-500/15 text-pink-700 dark:text-pink-400';
			default:
				return 'bg-muted text-foreground';
		}
	});

	const formattedData = $derived.by(() => {
		if (!token) return '';
		if (colorType === 'Unit') return '';
		if (colorType === 'Integer') return String(colorValue);
		if (colorType === 'Data') return JSON.stringify(colorValue, null, 2);
		return '';
	});

	const copyableData = $derived.by(() => {
		if (!token) return '';
		if (colorType === 'Unit') return 'Unit';
		if (colorType === 'Integer') return String(colorValue);
		if (colorType === 'Data') return JSON.stringify(colorValue, null, 2);
		return '';
	});

	function handleKeydown(e: KeyboardEvent) {
		if (e.key === 'Escape') {
			onClose();
		}
	}
</script>

<svelte:window onkeydown={handleKeydown} />

{#if open && token}
	<!-- Backdrop -->
	<div
		class="fixed inset-0 bg-black/20 z-40"
		style="right: 608px;"
		onclick={onClose}
		onkeydown={(e) => e.key === 'Escape' && onClose()}
		role="button"
		tabindex="-1"
		aria-label="Close sheet"
	></div>

	<!-- Sheet panel — fills canvas area below toolbar -->
	<div
		class="fixed left-0 bottom-0 z-50 bg-card border-r border-border shadow-2xl flex flex-col"
		style="right: 608px; top: 49px;"
	>
		<!-- Header -->
		<div class="flex items-center justify-between px-4 py-3 border-b border-border bg-muted">
			<div class="flex items-center gap-3">
				<h2 class="text-lg font-semibold text-foreground">Token</h2>
				<span class="px-2 py-0.5 text-xs font-medium rounded {typeBadgeClass}">
					{colorType}
				</span>
				<span class="text-sm text-muted-foreground">
					in <span class="font-medium text-foreground">{placeName}</span>
				</span>
			</div>
			<button
				onclick={onClose}
				class="p-1 rounded hover:bg-muted transition-colors"
				aria-label="Close"
			>
				<X class="w-5 h-5 text-muted-foreground" />
			</button>
		</div>

		<!-- Content -->
		<div class="flex-1 flex flex-col p-4 gap-4 min-h-0">
			<!-- Metadata -->
			<div class="shrink-0 grid grid-cols-2 gap-4">
				<div>
					<h3 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Token ID</h3>
					<div class="flex items-center gap-1">
						<span class="text-xs font-mono text-foreground/80 break-all">{token.id}</span>
						<CopyButton text={token.id} />
					</div>
				</div>
				<div>
					<h3 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Created</h3>
					<span class="text-sm text-foreground">
						{new Date(token.created_at).toLocaleString()}
					</span>
				</div>
			</div>

			<!-- Token Data -->
			<div class="flex-1 flex flex-col min-h-0">
				<div class="shrink-0 flex items-center justify-between mb-2">
					<h3 class="text-sm font-medium text-foreground/80">Token Data</h3>
					{#if colorType !== 'Unit'}
						<CopyButton text={copyableData} />
					{/if}
				</div>
				{#if colorType === 'Unit'}
					<div class="px-3 py-6 bg-muted rounded-lg text-center text-muted-foreground italic">
						Unit token (no data)
					</div>
				{:else if colorType === 'Integer'}
					<div class="px-4 py-6 bg-muted rounded-lg text-center">
						<span class="text-3xl font-mono font-bold text-primary">{colorValue}</span>
					</div>
				{:else if colorType === 'Data'}
					<div class="flex-1 min-h-0">
						<MonacoEditor value={formattedData} language="json" height="100%" readOnly />
					</div>
				{/if}
			</div>
		</div>
	</div>
{/if}
