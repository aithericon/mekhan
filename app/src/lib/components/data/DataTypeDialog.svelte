<script lang="ts">
	// Promote / edit dialog for registered data types. Promote names a schema
	// digest — the server derives the canonical columns from a live exemplar
	// entry, so there is no pre-submit column preview in v1 (promotion is
	// reversible). Edit renames / re-describes an existing type. Opened
	// imperatively via bind:this from the Schemas facet group and the
	// Data-types rail section.
	import type { CatalogueDataType } from '$lib/api/data';
	import type { DataTypesState } from './data-types.svelte';
	import { ApiError } from '$lib/api/client';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { Textarea } from '$lib/components/ui/textarea';
	import * as Dialog from '$lib/components/ui/dialog';
	import { toast } from 'svelte-sonner';

	let { datatypes }: { datatypes: DataTypesState } = $props();

	let open = $state(false);
	let mode = $state<'promote' | 'edit'>('promote');
	let digest = $state('');
	let editId = $state('');
	let name = $state('');
	let description = $state('');
	let formError = $state<string | null>(null);
	let saving = $state(false);

	export function openPromote(d: string) {
		mode = 'promote';
		digest = d;
		editId = '';
		name = '';
		description = '';
		formError = null;
		open = true;
	}

	export function openEdit(dt: CatalogueDataType) {
		mode = 'edit';
		editId = dt.id;
		digest = '';
		name = dt.name;
		description = dt.description ?? '';
		formError = null;
		open = true;
	}

	async function save() {
		const n = name.trim();
		if (!n) return;
		saving = true;
		formError = null;
		try {
			if (mode === 'promote') {
				await datatypes.promote({ digest, name: n, description: description.trim() || null });
				toast.success(`Registered data type “${n}”`);
			} else {
				await datatypes.update(editId, { name: n, description: description.trim() || null });
				toast.success(`Updated data type “${n}”`);
			}
			open = false;
		} catch (e) {
			if (e instanceof ApiError && e.status === 409) {
				toast.error(`A data type named “${n}” already exists`);
			} else {
				formError = e instanceof Error ? e.message : 'Save failed';
			}
		} finally {
			saving = false;
		}
	}
</script>

<Dialog.Root bind:open>
	<Dialog.Content class="sm:max-w-md">
		<Dialog.Header>
			<Dialog.Title>{mode === 'promote' ? 'Register data type' : 'Edit data type'}</Dialog.Title>
		</Dialog.Header>
		<div class="space-y-3">
			{#if mode === 'promote'}
				<div>
					<!-- svelte-ignore a11y_label_has_associated_control -->
					<label class="mb-1 block text-sm font-medium text-foreground">Schema digest</label>
					<p
						class="rounded border border-border bg-muted px-2 py-1.5 font-mono text-sm text-muted-foreground"
						data-testid="datatype-dialog-digest"
					>
						{digest}
					</p>
					<p class="mt-1 text-xs text-muted-foreground">
						The columns are derived server-side from a catalogued exemplar carrying this digest.
					</p>
				</div>
			{/if}
			<div>
				<!-- svelte-ignore a11y_label_has_associated_control -->
				<label class="mb-1 block text-sm font-medium text-foreground">Name</label>
				<Input
					bind:value={name}
					placeholder="sensor_readings"
					class="text-sm"
					data-testid="datatype-dialog-name"
				/>
			</div>
			<div>
				<!-- svelte-ignore a11y_label_has_associated_control -->
				<label class="mb-1 block text-sm font-medium text-foreground"
					>Description <span class="font-normal text-muted-foreground">(optional)</span></label
				>
				<Textarea bind:value={description} placeholder="What this shape means…" class="text-sm" />
			</div>
			{#if formError}<p class="text-sm text-red-600">{formError}</p>{/if}
		</div>
		<Dialog.Footer>
			<Button variant="ghost" onclick={() => (open = false)}>Cancel</Button>
			<Button
				onclick={save}
				disabled={saving || !name.trim()}
				data-testid="datatype-dialog-save"
			>
				{saving ? 'Saving…' : mode === 'promote' ? 'Register' : 'Save'}
			</Button>
		</Dialog.Footer>
	</Dialog.Content>
</Dialog.Root>
