<script lang="ts">
	import {
		Sheet,
		SheetContent,
		SheetTitle,
		SheetDescription
	} from '$lib/components/ui/sheet';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import { FormField } from '$lib/components/ui/form-field';
	import * as Select from '$lib/components/ui/select';
	import CheckCircle2 from '@lucide/svelte/icons/check-circle-2';
	import AlertCircle from '@lucide/svelte/icons/alert-circle';
	import Server from '@lucide/svelte/icons/server';
	import Loader2 from '@lucide/svelte/icons/loader-2';
	import { listResources, type ResourceSummary } from '$lib/api/resources';
	import {
		getTemplateRequirements,
		putTemplateBindings,
		bindingTierLabel,
		type TemplateRequirementsResponse,
		type SlotReadiness,
		type SlotBindingInput
	} from '$lib/api/template-bindings';

	type Props = {
		open: boolean;
		/** Template id (any version row). The bindings endpoint keys defaults by
		 *  the chain root, so any version works. */
		templateId: string;
		onclose: () => void;
		/** Fired after a successful save so the parent can refresh its readiness
		 *  banner / launch gate. */
		onsaved?: (next: TemplateRequirementsResponse) => void;
	};

	let { open, templateId, onclose, onsaved }: Props = $props();

	// ── Manifest + readiness ────────────────────────────────────────────────
	let manifest = $state<TemplateRequirementsResponse | null>(null);
	let loading = $state(false);
	let loadError = $state<string | null>(null);

	// Per-slot resource list, keyed by `resource_type` (loaded once per distinct
	// type the template needs). Shared across slots of the same type.
	let resourcesByType = $state<Record<string, ResourceSummary[]>>({});

	// The pending picker selections, keyed by slot_key. Seeded from the readiness
	// (a slot already satisfied by a workspace-default / override shows that
	// resource pre-selected; a baseline / platform / unbound slot starts empty so
	// the user opts in to a workspace default explicitly).
	let selections = $state<Record<string, string>>({});

	let saving = $state(false);
	let saveError = $state<string | null>(null);

	// (Re)load the manifest whenever the sheet opens for a (new) template.
	let loadedFor = $state('');
	$effect(() => {
		if (!open) return;
		const key = templateId;
		if (key === loadedFor && manifest) return;
		loadedFor = key;
		void load(key);
	});

	async function load(id: string) {
		loading = true;
		loadError = null;
		manifest = null;
		try {
			const res = await getTemplateRequirements(id);
			manifest = res;
			// Seed selections from any explicit (non-baseline, non-platform)
			// binding so the picker reflects the current workspace default.
			const seed: Record<string, string> = {};
			for (const r of res.readiness) {
				if (
					r.resource_id &&
					(r.tier === 'workspace_default' || r.tier === 'instance_override')
				) {
					seed[r.slot.key] = r.resource_id;
				}
			}
			selections = seed;
			// Load the resource lists for every distinct type the slots need.
			const types = [...new Set(res.slots.map((s) => s.resource_type))];
			await Promise.all(types.map((t) => loadResourceType(t)));
		} catch (e) {
			loadError = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	}

	async function loadResourceType(type: string) {
		if (resourcesByType[type]) return;
		try {
			const page = await listResources({ resource_type: type, perPage: 200 });
			resourcesByType = { ...resourcesByType, [type]: page.items };
		} catch {
			resourcesByType = { ...resourcesByType, [type]: [] };
		}
	}

	function setSelection(slotKey: string, resourceId: string) {
		selections = { ...selections, [slotKey]: resourceId };
	}

	function resourceLabel(type: string, resourceId: string | undefined): string {
		if (!resourceId) return 'Use default (platform / baseline)…';
		const found = (resourcesByType[type] ?? []).find((r) => r.id === resourceId);
		return found ? `${found.path} — ${found.display_name}` : resourceId;
	}

	function readinessFor(slotKey: string): SlotReadiness | undefined {
		return manifest?.readiness.find((r) => r.slot.key === slotKey);
	}

	async function save() {
		if (!manifest || saving) return;
		saving = true;
		saveError = null;
		const bindings: SlotBindingInput[] = Object.entries(selections)
			.filter(([, rid]) => !!rid)
			.map(([slot_key, resource_id]) => ({ slot_key, resource_id }));
		try {
			const next = await putTemplateBindings(templateId, bindings);
			manifest = next;
			onsaved?.(next);
		} catch (e) {
			saveError = e instanceof Error ? e.message : String(e);
		} finally {
			saving = false;
		}
	}

	const unboundRequiredCount = $derived(
		manifest
			? manifest.readiness.filter((r) => r.slot.required && !r.satisfied).length
			: 0
	);
</script>

<Sheet.Root
	{open}
	onOpenChange={(o: boolean) => {
		if (!o) onclose();
	}}
>
	<SheetContent class="flex w-full max-w-xl flex-col gap-0 p-0 sm:max-w-xl">
		<header class="flex items-start gap-3 border-b border-border px-5 py-4">
			<Server class="size-5 text-muted-foreground" />
			<div class="min-w-0">
				<SheetTitle>Configure resources</SheetTitle>
				<SheetDescription class="text-sm text-muted-foreground">
					Bind this template's resource slots for this workspace. Saved defaults
					apply to every run launched here; an individual run can still override
					them.
				</SheetDescription>
			</div>
		</header>

		<div class="flex-1 overflow-y-auto px-5 py-4 text-sm">
			{#if loading}
				<div class="flex items-center gap-2 text-muted-foreground">
					<Loader2 class="size-4 animate-spin" />
					Loading requirements…
				</div>
			{:else if loadError}
				<div class="rounded border border-destructive/40 bg-destructive/5 p-3 text-destructive">
					Failed to load requirements: {loadError}
				</div>
			{:else if manifest && manifest.slots.length === 0}
				<div class="rounded border border-border p-4 text-muted-foreground" data-testid="no-slots">
					This template references no resources or pools — there is nothing to bind.
				</div>
			{:else if manifest}
				{#if unboundRequiredCount > 0}
					<div
						class="mb-4 flex items-start gap-2 rounded border border-amber-200 bg-amber-50 p-3 text-amber-900 dark:border-amber-900/40 dark:bg-amber-950/30 dark:text-amber-200"
						data-testid="unbound-warning"
					>
						<AlertCircle class="mt-0.5 size-4 shrink-0" />
						<span>
							{unboundRequiredCount} required slot{unboundRequiredCount === 1 ? '' : 's'}
							{unboundRequiredCount === 1 ? 'is' : 'are'} unbound. A run can't launch in this
							workspace until {unboundRequiredCount === 1 ? 'it is' : 'they are'} bound.
						</span>
					</div>
				{/if}

				<ul class="space-y-3">
					{#each manifest.slots as slot (slot.key)}
						{@const r = readinessFor(slot.key)}
						<li
							class="rounded border border-border p-3"
							data-testid="slot-row"
							data-slot-key={slot.key}
						>
							<div class="mb-2 flex items-center justify-between gap-2">
								<div class="min-w-0">
									<div class="truncate font-medium">{slot.key}</div>
									<div class="mt-0.5 flex flex-wrap items-center gap-1.5 text-xs">
										<Badge variant="outline" class="font-mono">{slot.resource_type}</Badge>
										{#if slot.required}
											<span class="text-muted-foreground">required</span>
										{:else}
											<span class="text-muted-foreground">optional</span>
										{/if}
									</div>
								</div>
								{#if r?.satisfied}
									<span
										class="flex shrink-0 items-center gap-1 text-xs text-emerald-600 dark:text-emerald-400"
										data-testid="slot-satisfied"
									>
										<CheckCircle2 class="size-3.5" />
										{bindingTierLabel(r.tier) || 'Bound'}
									</span>
								{:else}
									<span
										class="flex shrink-0 items-center gap-1 text-xs text-amber-600 dark:text-amber-400"
										data-testid="slot-unbound"
									>
										<AlertCircle class="size-3.5" />
										Unbound
									</span>
								{/if}
							</div>

							<FormField label="Workspace default" for={`slot-${slot.key}`}>
								<Select.Root
									type="single"
									value={selections[slot.key] ?? ''}
									onValueChange={(v) => setSelection(slot.key, v ?? '')}
								>
									<Select.Trigger
										id={`slot-${slot.key}`}
										data-testid="select-slot-resource"
									>
										<span class="truncate text-sm">
											{resourceLabel(slot.resource_type, selections[slot.key])}
										</span>
									</Select.Trigger>
									<Select.Content>
										<Select.Item value="" label="Use default (platform / baseline)…" />
										{#each resourcesByType[slot.resource_type] ?? [] as res (res.id)}
											<Select.Item
												value={res.id}
												label={`${res.path} — ${res.display_name}`}
											/>
										{/each}
									</Select.Content>
								</Select.Root>
							</FormField>
							{#if (resourcesByType[slot.resource_type] ?? []).length === 0}
								<p class="mt-1 text-xs italic text-muted-foreground">
									No <code class="font-mono">{slot.resource_type}</code> resources in this
									workspace. Add one under <code class="font-mono">/resources</code>, or rely on
									a platform resource / the template baseline.
								</p>
							{/if}
							{#if r?.tier === 'platform_auto_bind'}
								<p class="mt-1 text-xs text-muted-foreground">
									Currently auto-bound to a platform resource. Pick a workspace resource above
									to override.
								</p>
							{:else if r?.tier === 'home_baseline'}
								<p class="mt-1 text-xs text-muted-foreground">
									Currently uses the template's baked-in baseline resource. Pick a workspace
									resource above to override.
								</p>
							{/if}
						</li>
					{/each}
				</ul>
			{/if}

			{#if saveError}
				<div class="mt-4 rounded border border-destructive/40 bg-destructive/5 p-3 text-destructive">
					Failed to save: {saveError}
				</div>
			{/if}
		</div>

		<footer class="flex items-center justify-between gap-2 border-t border-border px-5 py-3">
			<div class="text-xs text-muted-foreground">
				{#if manifest}
					{#if manifest.launchable}
						<span class="flex items-center gap-1 text-emerald-600 dark:text-emerald-400">
							<CheckCircle2 class="size-3.5" /> Ready to run
						</span>
					{:else}
						<span class="flex items-center gap-1 text-amber-600 dark:text-amber-400">
							<AlertCircle class="size-3.5" /> Not yet runnable
						</span>
					{/if}
				{/if}
			</div>
			<div class="flex gap-2">
				<Button variant="outline" onclick={onclose}>Close</Button>
				<Button
					onclick={save}
					disabled={saving || loading || !manifest || manifest.slots.length === 0}
					data-testid="save-bindings"
				>
					{#if saving}
						<Loader2 class="mr-1.5 size-4 animate-spin" />
						Saving…
					{:else}
						Save defaults
					{/if}
				</Button>
			</div>
		</footer>
	</SheetContent>
</Sheet.Root>
