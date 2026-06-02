<script lang="ts">
	// Node-level asset bindings (docs/20 §5). Lets the author stage one or more
	// scope-visible assets as ordinary inputs the node code reads (`<alias>.json`).
	// Shared between AutomatedStep and Agent nodes — both carry `assetBindings` on
	// their node data (a top-level `#[serde(default)]` array round-tripped through
	// graph-binding).
	//
	// Each row is an AssetBinding { refKey, alias }: `refKey` is picked via the
	// shared AssetPicker (a Select over scope-visible assets); `alias` is the
	// staged-input name (defaults to refKey, editable so two assets don't collide).
	// Persistence follows the repo's onchange idiom — we emit a fresh node-data
	// object via `onchange`; the assetBindings array rides graph-binding to Yjs.
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { FormField } from '$lib/components/ui/form-field';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import Database from '@lucide/svelte/icons/database';
	import AssetPicker from './shared/AssetPicker.svelte';
	import type { AssetBinding, ScopeContext } from '$lib/api/assets';

	type Props = {
		/** Current bindings (may be undefined for legacy nodes). */
		bindings: AssetBinding[] | undefined;
		readonly?: boolean;
		onchange: (bindings: AssetBinding[]) => void;
		/** When set, scope asset resolution to this template (downward visibility). */
		templateId?: string;
	};

	let { bindings, readonly = false, onchange, templateId }: Props = $props();

	const rows = $derived<AssetBinding[]>(bindings ?? []);

	// Scope the picker to the template when we know it; otherwise list the
	// caller's workspace assets.
	const scope = $derived<ScopeContext>(
		templateId ? { kind: 'template', id: templateId } : { kind: 'workspace' }
	);

	function addRow() {
		onchange([...rows, { refKey: '', alias: '' }]);
	}

	function removeRow(i: number) {
		onchange(rows.filter((_, idx) => idx !== i));
	}

	function setRefKey(i: number, refKey: string) {
		onchange(
			rows.map((b, idx) => {
				if (idx !== i) return b;
				// Default the alias to the ref-key when the author hasn't set one
				// (or had it tracking the previous ref-key).
				const alias = !b.alias || b.alias === b.refKey ? refKey : b.alias;
				return { refKey, alias };
			})
		);
	}

	function setAlias(i: number, alias: string) {
		onchange(rows.map((b, idx) => (idx === i ? { ...b, alias } : b)));
	}
</script>

<div class="space-y-2 border-t border-border/40 pt-3">
	<div class="flex items-center justify-between">
		<div class="flex items-center gap-1.5">
			<Database class="size-3.5 text-muted-foreground" />
			<span class="text-sm font-medium text-muted-foreground">Asset bindings</span>
		</div>
		{#if !readonly}
			<Button
				variant="outline"
				size="sm"
				class="h-7 gap-1 px-2 text-sm"
				onclick={addRow}
				data-testid="asset-binding-add"
			>
				<Plus class="size-3.5" />
				Bind asset
			</Button>
		{/if}
	</div>

	{#if rows.length === 0}
		<p class="text-sm text-muted-foreground">
			Stage curated <a href="/assets" class="underline">assets</a> as inputs. Each bound asset's
			records arrive as <code class="font-mono">&lt;alias&gt;.json</code>; File fields stage alongside.
		</p>
	{:else}
		<div class="space-y-3">
			{#each rows as binding, i (i)}
				<div class="space-y-2 rounded-md border border-border/60 p-3">
					<div class="flex items-start gap-2">
						<div class="min-w-0 flex-1">
							<AssetPicker
								selected={binding.refKey}
								onChange={(rk) => setRefKey(i, rk)}
								{scope}
								{readonly}
								label="Asset"
								testId={`asset-binding-picker-${i}`}
							/>
						</div>
						<button
							type="button"
							class="mt-7 rounded p-1 text-muted-foreground transition-colors hover:text-destructive disabled:opacity-30"
							disabled={readonly}
							onclick={() => removeRow(i)}
							title="Remove binding"
						>
							<Trash2 class="size-3.5" />
						</button>
					</div>
					<FormField label="Alias (staged input name)" for={`asset-binding-alias-${i}`}>
						<Input
							id={`asset-binding-alias-${i}`}
							value={binding.alias}
							placeholder={binding.refKey || 'alias'}
							disabled={readonly}
							class="font-mono text-sm"
							data-testid={`asset-binding-alias-${i}`}
							oninput={(e) => setAlias(i, (e.currentTarget as HTMLInputElement).value)}
						/>
					</FormField>
				</div>
			{/each}
		</div>
	{/if}
</div>
