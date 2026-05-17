<script lang="ts">
	import { Separator } from '$lib/components/ui/separator';
	import { Card } from '$lib/components/ui/card';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import { Textarea } from '$lib/components/ui/textarea';
	import NodeKindBadge from '../NodeKindBadge.svelte';
	import { isLeaseToken } from '$lib/petri/token-analysis';
	import type { PlaceDetails } from '$lib/stores/inspector-selectors';

	interface Props {
		placeDetails: PlaceDetails;
		loading?: boolean;
		injectJsonInput: string;
		injectError: string | null;
		injectSuccess: boolean;
		onSelectToken?: (placeId: string, tokenId: string) => void;
		onInjectToken?: () => void;
		onInjectInput: (value: string) => void;
	}

	let {
		placeDetails,
		loading = false,
		injectJsonInput,
		injectError,
		injectSuccess,
		onSelectToken,
		onInjectToken,
		onInjectInput
	}: Props = $props();
</script>

<div class="space-y-4">
	<Card tone="muted">
		<h3 class="text-lg font-medium text-foreground">{placeDetails.place.name}</h3>
		<p class="text-xs text-muted-foreground font-mono">{placeDetails.place.id}</p>
		<div class="flex items-center gap-2 mt-2">
			<NodeKindBadge kind={(placeDetails.place.kind ?? 'place') as any} />
			{#if placeDetails.place.capacity}
				<span class="text-xs text-muted-foreground">
					Capacity: <span class="font-medium">{placeDetails.place.capacity}</span>
				</span>
			{/if}
		</div>
	</Card>

	<Separator />

	<!-- Tokens List -->
	<Card tone="muted">
		<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-2">
			Tokens ({placeDetails.tokens.length})
		</h4>
		{#if placeDetails.tokens.length === 0}
			<p class="text-sm text-muted-foreground italic">No tokens</p>
		{:else}
			<div class="space-y-2 max-h-48 overflow-y-auto">
				{#each placeDetails.tokens as token (token.id)}
					<button
						class="w-full text-left p-2 rounded border transition-colors {isLeaseToken(token)
							? 'border-l-2 border-l-warning border-warning/30 bg-warning/10 hover:border-warning/50 hover:bg-warning/20'
							: 'border-l-2 border-l-primary/50 border-border hover:border-primary/50 hover:bg-primary/10'}"
						onclick={() => onSelectToken?.(placeDetails.place.id, token.id)}
					>
						<div class="flex items-start gap-2">
							<Badge variant="muted" size="xs" class="shrink-0">{token.color.type}</Badge>
							{#if isLeaseToken(token)}
								<NodeKindBadge kind="lease" size="xs" class="shrink-0" />
							{/if}
							<div class="flex-1 min-w-0">
								{#if token.color.type === 'Unit'}
									<span class="text-sm text-muted-foreground italic">empty</span>
								{:else if token.color.type === 'Integer'}
									<span class="text-sm font-mono text-primary font-medium">{token.color.value}</span>
								{:else if token.color.type === 'Data'}
									<pre class="text-xs text-foreground/80 truncate">{JSON.stringify(token.color.value)}</pre>
								{/if}
							</div>
						</div>
						<div class="text-[10px] font-mono text-muted-foreground mt-1 truncate">{token.id.slice(0, 8)}...</div>
					</button>
				{/each}
			</div>
		{/if}
	</Card>

	<Separator />

	<!-- Token Injection -->
	<div>
		<h4 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-2">Inject Token</h4>
		<Textarea
			value={injectJsonInput}
			oninput={(e) => onInjectInput((e.currentTarget as HTMLTextAreaElement).value)}
			placeholder={'{"amount": 500}'}
			class="h-20 font-mono text-sm resize-none"
			spellcheck="false"
		/>
		{#if injectError}
			<p class="text-xs text-destructive mt-1">{injectError}</p>
		{/if}
		{#if injectSuccess}
			<p class="text-xs text-success mt-1">Token injected!</p>
		{/if}
		<Button onclick={() => onInjectToken?.()} disabled={loading} size="sm" class="mt-2 w-full">
			{loading ? 'Injecting...' : 'Inject Token'}
		</Button>
	</div>
</div>
