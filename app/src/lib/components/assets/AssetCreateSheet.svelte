<script lang="ts">
	// Asset CREATE flow (docs/20 §4.2). Replaces the old `prompt()` + `confirm()`
	// chain with a proper sheet that mirrors the resource create sheet: pick the
	// type, name the asset, place it (shared PlacementFields: Location + Private),
	// then create. On success the parent opens the records editor so the author
	// flows straight into populating rows — same two-step the old prompt did, but
	// as a real form.
	import { Sheet, SheetContent, SheetTitle, SheetDescription, SheetClose } from '$lib/components/ui/sheet';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { Badge } from '$lib/components/ui/badge';
	import { FormField } from '$lib/components/ui/form-field';
	import * as Select from '$lib/components/ui/select';
	import X from '@lucide/svelte/icons/x';
	import PlacementFields from '$lib/components/iam/PlacementFields.svelte';
	import {
		createAsset,
		type AssetSummary,
		type AssetTypeSummary,
		type ScopeContext
	} from '$lib/api/assets';

	type Props = {
		open: boolean;
		/** Asset types available in the current scope — the parent already loaded
		 *  these, so the sheet doesn't re-fetch. */
		types: AssetTypeSummary[];
		/** Type to preselect (the parent's active type filter, if any). */
		prefillTypeId?: string;
		/** Default placement: the folder the user is browsing (tree selection), or
		 *  workspace when at the root. */
		defaultScope?: ScopeContext;
		/** Called with the freshly-created asset so the parent can open the records
		 *  editor and refresh its list. */
		oncreated: (asset: AssetSummary) => void;
	};

	let {
		open = $bindable(),
		types,
		prefillTypeId,
		defaultScope,
		oncreated
	}: Props = $props();

	let typeId = $state('');
	let refKey = $state('');
	let displayName = $state('');
	let scope = $state<ScopeContext>({ kind: 'workspace' });
	let restricted = $state(false);
	let saving = $state(false);
	let error = $state<string | null>(null);

	// Reset/prefill each time the sheet opens. Initialized in an effect (not via
	// `$state(prop)`) to avoid capturing only the first prop value.
	$effect(() => {
		if (!open) return;
		error = null;
		typeId = prefillTypeId && types.some((t) => t.id === prefillTypeId) ? prefillTypeId : (types[0]?.id ?? '');
		refKey = '';
		displayName = '';
		scope = defaultScope ?? { kind: 'workspace' };
		restricted = false;
	});

	const selectedType = $derived(types.find((t) => t.id === typeId) ?? null);

	// Mirror the server-side ref-key grammar (^[a-z][a-z0-9_]*$). Surfaced inline
	// so the user sees the constraint before the 400 round-trip.
	const REF_KEY_PATTERN = /^[a-z][a-z0-9_]*$/;
	const refKeyError = $derived.by(() => {
		if (!refKey) return null;
		if (!REF_KEY_PATTERN.test(refKey)) {
			return 'Lowercase letter first, then letters / digits / underscores (e.g. steel).';
		}
		return null;
	});

	async function submit() {
		if (!typeId) {
			error = 'Choose an asset type.';
			return;
		}
		const key = refKey.trim();
		if (!key) {
			error = 'Enter a ref-key.';
			return;
		}
		if (refKeyError) {
			error = refKeyError;
			return;
		}
		saving = true;
		error = null;
		try {
			const created = await createAsset({
				type_id: typeId,
				ref_key: key,
				display_name: displayName.trim() || key,
				scope_kind: scope.kind,
				scope_id: scope.kind === 'workspace' ? null : scope.id,
				restricted
			});
			open = false;
			oncreated(created);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to create asset';
		} finally {
			saving = false;
		}
	}
</script>

<Sheet.Root bind:open>
	<SheetContent class="w-[520px] sm:max-w-[520px]">
		<div class="flex items-center justify-between border-b border-border px-5 py-4">
			<div>
				<SheetTitle class="text-lg font-semibold">New asset</SheetTitle>
				<SheetDescription class="text-sm text-muted-foreground">
					A typed collection of records. Name it, place it, then add rows.
				</SheetDescription>
			</div>
			<SheetClose>
				<X class="size-4" />
			</SheetClose>
		</div>

		<div class="flex flex-1 flex-col overflow-y-auto px-5 py-4">
			{#if error}
				<div class="mb-4 rounded-md border border-destructive/30 bg-destructive/10 px-3 py-2 text-sm text-destructive">
					{error}
				</div>
			{/if}

			{#if types.length === 0}
				<p class="rounded-md border border-dashed border-border px-4 py-8 text-center text-sm text-muted-foreground">
					Define an asset type first — a schema (a list of typed fields) the
					records conform to.
				</p>
			{:else}
				<div class="space-y-4">
					<FormField label="Type">
						<Select.Root type="single" value={typeId} onValueChange={(v) => (typeId = v ?? '')}>
							<Select.Trigger class="w-full" data-testid="asset-create-type">
								{#if selectedType}
									<span class="flex items-center gap-2">
										<span>{selectedType.display_name}</span>
										<Badge variant="secondary">{selectedType.cardinality}</Badge>
									</span>
								{:else}
									— select a type —
								{/if}
							</Select.Trigger>
							<Select.Content>
								{#each types as t (t.id)}
									<Select.Item value={t.id} label={t.display_name} />
								{/each}
							</Select.Content>
						</Select.Root>
					</FormField>

					<FormField
						label="Ref-key"
						description="Snake_case identifier. How workflow nodes bind this asset."
					>
						<Input
							type="text"
							value={refKey}
							placeholder="steel"
							oninput={(e) => (refKey = (e.currentTarget as HTMLInputElement).value)}
							aria-invalid={refKeyError ? 'true' : undefined}
							class="font-mono text-sm"
							data-testid="asset-create-refkey"
						/>
						{#if refKeyError}
							<p class="mt-1 text-sm text-destructive" data-testid="asset-create-refkey-error">
								{refKeyError}
							</p>
						{/if}
					</FormField>

					<FormField label="Display name">
						<Input
							type="text"
							value={displayName}
							placeholder={refKey || 'Optional label'}
							oninput={(e) => (displayName = (e.currentTarget as HTMLInputElement).value)}
							class="text-sm"
						/>
					</FormField>

					<PlacementFields bind:scope bind:restricted testidPrefix="asset-create" />
				</div>
			{/if}
		</div>

		<div class="flex items-center justify-end gap-2 border-t border-border bg-muted/30 px-5 py-3">
			<Button variant="ghost" size="sm" onclick={() => (open = false)}>Cancel</Button>
			<Button
				size="sm"
				onclick={submit}
				disabled={saving || types.length === 0 || !typeId}
				data-testid="asset-create-submit"
			>
				{saving ? 'Creating…' : 'Create asset'}
			</Button>
		</div>
	</SheetContent>
</Sheet.Root>
