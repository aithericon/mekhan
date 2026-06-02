<script lang="ts">
	// Admin list of capability types. Loads from GET /api/v1/capability-types,
	// shows a table of name + field count + field summary, lets admins create
	// (via CapabilityTypeEditModal) and revoke (with a confirm dialog).
	// The mint + revoke controls are hidden when the session user lacks the
	// 'admin' role (the backend enforces regardless — this is UX-only).
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import RotateCcw from '@lucide/svelte/icons/rotate-ccw';
	import {
		listCapabilityTypes,
		revokeCapabilityType,
		type CapabilityTypeSummary
	} from '$lib/api/capability-types';
	import { auth } from '$lib/auth/store.svelte';
	import CapabilityTypeEditModal from './CapabilityTypeEditModal.svelte';

	const isAdmin = $derived(auth.session?.user.roles.includes('admin') ?? false);

	let items = $state<CapabilityTypeSummary[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let modalOpen = $state(false);

	async function load() {
		loading = true;
		error = null;
		try {
			const page = await listCapabilityTypes({ perPage: 200 });
			items = page.items;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load capability types';
			items = [];
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		load();
	});

	async function handleRevoke(id: string, name: string) {
		if (
			!confirm(
				`Revoke capability type "${name}"? Existing runners that advertise it are unaffected until they re-register.`
			)
		)
			return;
		try {
			await revokeCapabilityType(id);
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to revoke';
		}
	}

	function onSaved() {
		modalOpen = false;
		load();
	}

	/** Human-readable field summary for the table row. */
	function fieldSummary(ct: CapabilityTypeSummary): string {
		if (ct.fields.length === 0) return '—';
		return ct.fields
			.slice(0, 4)
			.map((f) => `${f.name}:${f.kind}`)
			.join(', ')
			.concat(ct.fields.length > 4 ? `, +${ct.fields.length - 4} more` : '');
	}

	const formatDate = (s: string) => new Date(s).toLocaleString();
</script>

<div class="space-y-4" data-testid="capability-types-list">
	<div class="flex items-center justify-between">
		<span class="text-sm text-muted-foreground">
			{items.length} type{items.length === 1 ? '' : 's'}
		</span>
		{#if isAdmin}
			<Button
				variant="default"
				size="sm"
				onclick={() => (modalOpen = true)}
				class="gap-1.5"
				data-testid="cap-type-create-button"
			>
				<Plus class="size-4" />
				New capability type
			</Button>
		{/if}
	</div>

	{#if error}
		<div
			class="rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800"
			data-testid="cap-type-error"
		>
			{error}
			<Button variant="ghost" size="sm" onclick={load} class="ml-2 gap-1">
				<RotateCcw class="size-3" />
				Retry
			</Button>
		</div>
	{/if}

	{#if loading}
		<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">
			Loading…
		</div>
	{:else if items.length === 0}
		<div
			class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-16"
		>
			<p class="mt-3 text-sm text-muted-foreground">No capability types defined</p>
			<p class="text-sm text-muted-foreground">
				Capability types define the typed schemas runners can advertise.
			</p>
			{#if isAdmin}
				<Button
					variant="outline"
					size="sm"
					class="mt-4 gap-1.5"
					onclick={() => (modalOpen = true)}
				>
					<Plus class="size-4" />
					Create your first capability type
				</Button>
			{/if}
		</div>
	{:else}
		<div class="rounded-lg border border-border overflow-hidden">
			<table class="w-full text-sm" data-testid="cap-type-table">
				<thead>
					<tr class="border-b border-border bg-muted/40">
						<th class="px-4 py-3 text-left font-medium text-muted-foreground">Name</th>
						<th class="px-4 py-3 text-left font-medium text-muted-foreground">Fields</th>
						<th class="px-4 py-3 text-left font-medium text-muted-foreground">Field summary</th>
						<th class="px-4 py-3 text-left font-medium text-muted-foreground">Created</th>
						{#if isAdmin}
							<th class="px-4 py-3 text-right font-medium text-muted-foreground">Actions</th>
						{/if}
					</tr>
				</thead>
				<tbody>
					{#each items as ct (ct.id)}
						<tr
							class="border-b border-border last:border-0 hover:bg-accent/30 transition-colors"
							data-testid="cap-type-row-{ct.id}"
						>
							<td class="px-4 py-3">
								<span class="font-mono font-medium text-foreground">{ct.name}</span>
							</td>
							<td class="px-4 py-3">
								<Badge variant="secondary">{ct.fields.length}</Badge>
							</td>
							<td class="px-4 py-3 text-muted-foreground max-w-xs truncate">
								{fieldSummary(ct)}
							</td>
							<td class="px-4 py-3 text-muted-foreground whitespace-nowrap">
								{formatDate(ct.created_at)}
							</td>
							{#if isAdmin}
								<td class="px-4 py-3 text-right">
									<Button
										variant="ghost"
										size="sm"
										class="text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
										onclick={() => handleRevoke(ct.id, ct.name)}
										aria-label="Revoke {ct.name}"
										data-testid="cap-type-revoke-{ct.id}"
									>
										<Trash2 class="size-3.5" />
									</Button>
								</td>
							{/if}
						</tr>
					{/each}
				</tbody>
			</table>
		</div>
	{/if}
</div>

{#if isAdmin}
	<CapabilityTypeEditModal bind:open={modalOpen} onsaved={onSaved} />
{/if}
