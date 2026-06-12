<script lang="ts">
	// Shared "move this object to a different scope" control for the asset /
	// asset-type / resource edit sheets. Reuses the asset-layer ScopeSelector
	// (workspace ▸ folder cascade); on change it calls `onMove` with the chosen
	// scope and surfaces busy / error inline. The parent owns the actual PATCH
	// (assets.moveAsset / moveAssetType / resources.moveResource) and the
	// post-move refresh — this is purely the placement picker + status.
	import { FormField } from '$lib/components/ui/form-field';
	import ScopeSelector from '$lib/components/assets/ScopeSelector.svelte';
	import type { ScopeContext } from '$lib/api/assets';

	let {
		scope,
		onMove,
		disabled = false,
		testid = 'move-location'
	}: {
		/** The object's current owner scope (controlled by the parent). */
		scope: ScopeContext;
		/** Persist a move to `next`. Throwing surfaces the message inline. */
		onMove: (next: ScopeContext) => Promise<void>;
		disabled?: boolean;
		testid?: string;
	} = $props();

	let busy = $state(false);
	let error = $state<string | null>(null);

	function sameScope(a: ScopeContext, b: ScopeContext): boolean {
		if (a.kind !== b.kind) return false;
		return a.kind === 'workspace' || a.id === (b as { id: string }).id;
	}

	async function change(next: ScopeContext) {
		if (busy || sameScope(next, scope)) return;
		busy = true;
		error = null;
		try {
			await onMove(next);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Move failed';
		} finally {
			busy = false;
		}
	}
</script>

<FormField
	label="Location"
	description="Move this to a different folder or the workspace root. Access then inherits from the new location."
>
	<div data-testid={testid}>
		<ScopeSelector value={scope} onChange={change} readonly={disabled || busy} />
		{#if error}
			<p class="mt-1.5 text-sm text-destructive" data-testid="{testid}-error">{error}</p>
		{/if}
	</div>
</FormField>
