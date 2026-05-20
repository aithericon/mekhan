<script lang="ts">
	import { Button } from '$lib/components/ui/button';
	import type { TokenRef, ProducedTokenRef } from '$lib/stores/inspector-selectors';

	// One of the three near-identical "Consumed / Produced / Read tokens"
	// blocks that were copy-pasted across the TransitionFired / EffectCompleted
	// / EffectFailed inspector branches.
	type Kind = 'consumed' | 'produced' | 'read';

	interface Props {
		kind: Kind;
		refs: (TokenRef | ProducedTokenRef)[];
		onSelectPlace?: (id: string) => void;
		onSelectToken?: (placeId: string, tokenId: string) => void;
		onViewToken?: () => void;
	}

	let { kind, refs, onSelectPlace, onSelectToken, onViewToken }: Props = $props();

	const HEADINGS: Record<Kind, string> = {
		consumed: 'Consumed',
		produced: 'Produced',
		read: 'Read'
	};
	const SYMBOLS: Record<Kind, string> = { consumed: '-', produced: '+', read: '○' };
	const SYMBOL_CLASS: Record<Kind, string> = {
		consumed: 'text-destructive',
		produced: 'text-success',
		read: 'text-info'
	};

	// Consumed refs key on `tokenId`; produced/read key on `token.id`.
	function tokenId(ref: TokenRef | ProducedTokenRef): string {
		return 'tokenId' in ref ? ref.tokenId : ref.token.id;
	}
</script>

{#if refs && refs.length > 0}
	<div>
		<h4 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground mb-1">
			{HEADINGS[kind]} ({refs.length})
		</h4>
		<div class="space-y-0.5">
			{#each refs as ref (tokenId(ref))}
				<div class="flex items-center gap-2 text-sm">
					<span class={SYMBOL_CLASS[kind]}>{SYMBOLS[kind]}</span>
					<Button variant="link" size="inline" onclick={() => onSelectPlace?.(ref.placeId)}>
						{ref.placeName}
					</Button>
					<Button
						variant="link"
						size="inline"
						class="text-muted-foreground font-mono hover:text-primary"
						onclick={() => {
							onSelectToken?.(ref.placeId, tokenId(ref));
							onViewToken?.();
						}}
					>
						{tokenId(ref).slice(0, 8)}
					</Button>
				</div>
			{/each}
		</div>
	</div>
{/if}
