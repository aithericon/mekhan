<script lang="ts">
	import { Card } from '$lib/components/ui/card';
	import { CopyButton } from '$lib/components/ui/copy-button';
	import { Button } from '$lib/components/ui/button';
	import type { SelectedElement } from '$lib/types/petri';
	import type { TokenDetails } from '$lib/stores/inspector-selectors';

	interface Props {
		tokenDetails: TokenDetails;
		selectedElement: SelectedElement;
		previousSelection: SelectedElement;
		onSelectPlace?: (id: string) => void;
		onSelectEvent?: (sequence: number) => void;
		onViewToken?: () => void;
	}

	let {
		tokenDetails,
		selectedElement,
		previousSelection,
		onSelectPlace,
		onSelectEvent,
		onViewToken
	}: Props = $props();
</script>

<div class="space-y-4">
	<Card tone="muted">
		<Button
			variant="link"
			size="inline"
			class="text-sm mb-2"
			onclick={() => {
				if (previousSelection?.type === 'event') {
					onSelectEvent?.(previousSelection.sequence);
				} else if (selectedElement?.type === 'token') {
					onSelectPlace?.(selectedElement.placeId);
				}
			}}
		>
			&larr; {previousSelection?.type === 'event' ? `Back to Event #${previousSelection.sequence}` : `Back to ${tokenDetails.placeName}`}
		</Button>
		<h3 class="text-lg font-medium text-foreground">Token</h3>
		<p class="text-xs text-muted-foreground font-mono">{tokenDetails.token.id}</p>
	</Card>

	<div class="flex items-center gap-2">
		{#if tokenDetails.token.color.type !== 'Unit'}
			<CopyButton text={tokenDetails.token.color.type === 'Integer' ? String(tokenDetails.token.color.value) : JSON.stringify(tokenDetails.token.color.value, null, 2)} />
		{/if}
		{#if onViewToken}
			<Button onclick={onViewToken} class="flex-1">
				View Details
			</Button>
		{/if}
	</div>
</div>
