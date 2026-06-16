<script lang="ts">
	// Control-Plane capacity modal — creates OR edits one capacity. A kind
	// switcher across the four ways an operator adds dispatch capacity:
	//
	//   runner group — a presence `capacity` (the `instrument` preset). Name only.
	//   limit        — a seeded `capacity` (the `limit` preset). Name + count N.
	//   worker pool  — a pull-queue `capacity` (the `worker` preset). Name only.
	//   cluster      — a `datacenter` resource (scheduler backend). Full schema
	//                  form via <SchemaFields> (scheduler_flavor discriminator +
	//                  ssh/nomad/http + secrets).
	//
	// The three `capacity` kinds POST { path, resource_type: 'capacity',
	// config: { preset, capacity_amount? } } — the backend expands the named preset
	// into its locked axes and lets the one free field (the limit's count) override.
	// The cluster kind POSTs { path, resource_type: 'datacenter', config } built
	// from the schema form (string model coerced at submit, mirroring
	// ResourceEditModal's buildConfig).
	//
	// EDIT mode (the `editing` prop set to an existing capacity): the kind is
	// derived from its backend and LOCKED (you can't morph a runner group into a
	// cluster), the path is locked (it's the binding alias / identity), and the
	// editable fields are prefilled — the limit's count from `live.seeded`, the
	// cluster's schema fields from `getResource(id).public_config` (secrets stay
	// blank = "keep current"). Submit routes through `updateResource`, which
	// re-expands the preset, re-validates the axes, and re-deploys the backing net.
	import {
		Sheet,
		SheetContent,
		SheetTitle,
		SheetDescription,
		SheetClose
	} from '$lib/components/ui/sheet';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { FormField } from '$lib/components/ui/form-field';
	import Server from '@lucide/svelte/icons/server';
	import KeyRound from '@lucide/svelte/icons/key-round';
	import Cpu from '@lucide/svelte/icons/cpu';
	import Boxes from '@lucide/svelte/icons/boxes';
	import Users from '@lucide/svelte/icons/users';
	import SchemaFields, {
		deriveFieldSpecs,
		type FieldSpec
	} from '$lib/components/resources/SchemaFields.svelte';
	import {
		createResource,
		getResource,
		updateResource,
		listResourceTypes,
		type ResourceTypeInfo
	} from '$lib/api/resources';
	import type { CapacitySummary } from '$lib/api/capacities';
	import { resolveEditKind, type Kind } from './new-capacity-kind';
	import { auth } from '$lib/auth/store.svelte';
	import { isPlatformCapacity } from '$lib/api/resource-tier';
	import Globe from '@lucide/svelte/icons/globe';

	// ── Kind switcher ───────────────────────────────────────────────────────────

	type Props = {
		open: boolean;
		/** Optional pre-loaded type list (the page already fetched it). */
		types?: ResourceTypeInfo[];
		/** When set, the modal edits this capacity instead of creating: kind +
		 *  path locked, fields prefilled. `null`/absent ⇒ create. */
		editing?: CapacitySummary | null;
		/** Called after a successful create or update (parent closes + refreshes). */
		onsaved: () => void;
	};

	let { open = $bindable(), types: typesProp = [], editing = null, onsaved }: Props = $props();

	const isEdit = $derived(editing !== null);
	const KINDS: { kind: Kind; label: string; preset?: string; hint: string }[] = [
		{
			kind: 'runner_group',
			label: 'Machine pool',
			preset: 'instrument',
			hint: 'Presence-driven — one unit per live runner. Enroll runners into it.'
		},
		{
			kind: 'limit',
			label: 'Concurrency limit',
			preset: 'limit',
			hint: 'A seeded fixed-count token pool. Steps claim a token to run.'
		},
		{
			kind: 'worker',
			label: 'Worker pool',
			preset: 'worker',
			hint: "Competing-consumer workers that pull from this group's queue. Enroll workers into it."
		},
		{
			kind: 'cluster',
			label: 'Cluster',
			hint: 'A scheduler datacenter (Slurm / Nomad / HTTP) leasing allocations.'
		},
		{
			kind: 'human',
			label: 'Human pool',
			preset: 'human',
			hint: 'Pool of people — members consent to (claim) offered work. Enroll members after creating.'
		}
	];
	const KIND_ICON: Record<Kind, typeof Server> = {
		runner_group: Server,
		limit: KeyRound,
		worker: Cpu,
		cluster: Boxes,
		human: Users
	};

	let kind = $state<Kind>('runner_group');
	const activeKind = $derived(KINDS.find((k) => k.kind === kind)!);

	// ── Platform tier ────────────────────────────────────────────────────────────
	// A pool can be created at the shared platform tier (`scope_kind: 'platform'`)
	// instead of the caller's workspace — visible read-only to every workspace,
	// curated by platform admins. The toggle is offered on CREATE to platform
	// admins only; the backend independently 403s a non-admin platform mint. On
	// EDIT we don't move tiers — we just badge a platform pool as read-only-tier.
	const isPlatformAdmin = $derived(auth.isPlatformAdmin);
	let createAsPlatform = $state(false);
	const editIsPlatform = $derived(isEdit && !!editing && isPlatformCapacity(editing));

	// ── Shared form state ───────────────────────────────────────────────────────
	let path = $state('');
	let displayName = $state('');
	// `limit` count (the one free axis the preset exposes).
	let count = $state('1');
	// Cluster (datacenter) schema-form state.
	let fieldValues = $state<Record<string, string>>({});
	let discriminator = $state<string | null>(null);

	let types = $state<ResourceTypeInfo[]>([]);
	let loading = $state(false);
	let error = $state<string | null>(null);

	const datacenterDescriptor = $derived<ResourceTypeInfo | null>(
		types.find((t) => t.name === 'datacenter') ?? null
	);

	// Field specs for the cluster (datacenter) schema — used to coerce the string
	// model into the typed config at submit (mirrors ResourceEditModal.buildConfig).
	const clusterFieldSpecs = $derived.by<FieldSpec[]>(() => {
		if (!datacenterDescriptor) return [];
		return deriveFieldSpecs(
			(datacenterDescriptor.schema ?? {}) as Record<string, unknown>,
			datacenterDescriptor.secret_fields,
			[...datacenterDescriptor.public_fields, ...datacenterDescriptor.secret_fields],
			discriminator ? fieldValues[discriminator] : undefined
		);
	});

	// Same snake_case identifier grammar the backend enforces on `path`.
	const PATH_PATTERN = /^[a-z][a-z0-9_]*$/;
	const pathError = $derived.by(() => {
		if (!path) return null;
		if (!PATH_PATTERN.test(path)) {
			return 'Lowercase letter first, then letters / digits / underscores.';
		}
		return null;
	});

	// ── Bootstrap on open ─────────────────────────────────────────────────────────
	$effect(() => {
		if (!open) return;
		// `editing` is read so the effect re-runs when the parent swaps the target.
		const target = editing;
		error = null;
		// Reset the form each open (create defaults; overwritten below on edit).
		// `presence` is ambiguous — runner groups AND human pools share it — so
		// `resolveEditKind` peeks at the acceptance axis (`consent` ⇒ human).
		kind = target ? resolveEditKind(target) : 'runner_group';
		path = target ? target.path : '';
		displayName = target ? target.display_name : '';
		// Limit count: the seeded N is already on the summary's live facet.
		count = target && target.live.kind === 'tokens' ? String(target.live.seeded) : '1';
		createAsPlatform = false;
		fieldValues = {};
		discriminator = null;
		(async () => {
			if (types.length === 0) {
				types = typesProp.length > 0 ? typesProp : await listResourceTypes().catch((e) => {
					error = e instanceof Error ? e.message : 'Failed to load types';
					return [];
				});
			}
			// Cluster edit: pull the datacenter's current public config into the
			// schema form (secret fields stay blank ⇒ "keep current" on submit).
			if (target && target.backend === 'scheduler') {
				loading = true;
				try {
					const detail = await getResource(target.id);
					const values: Record<string, string> = {};
					const config = (detail.public_config ?? {}) as Record<string, unknown>;
					for (const [k, v] of Object.entries(config)) {
						values[k] = v === null || v === undefined ? '' : String(v);
					}
					for (const f of detail.redacted_secret_fields) values[f] = '';
					fieldValues = values;
				} catch (e) {
					error = e instanceof Error ? e.message : 'Failed to load cluster';
				} finally {
					loading = false;
				}
			}
		})();
	});

	// ── Submit ────────────────────────────────────────────────────────────────────
	function buildClusterConfig(): Record<string, unknown> {
		const out: Record<string, unknown> = {};
		for (const spec of clusterFieldSpecs) {
			const raw = fieldValues[spec.name] ?? '';
			// Edit: a blank secret means "keep the current Vault value" — omit it
			// so the update doesn't clobber it (mirrors ResourceEditModal).
			if (isEdit && spec.isSecret && raw === '') continue;
			if (raw === '' && !spec.isRequired) continue;
			if (spec.jsonType === 'integer') {
				const n = parseInt(raw, 10);
				out[spec.name] = Number.isFinite(n) ? n : 0;
			} else if (spec.jsonType === 'number') {
				const n = parseFloat(raw);
				out[spec.name] = Number.isFinite(n) ? n : 0;
			} else if (spec.jsonType === 'boolean') {
				out[spec.name] = raw === 'true';
			} else {
				out[spec.name] = raw;
			}
		}
		return out;
	}

	async function submit() {
		if (!path) {
			error = 'Enter a name';
			return;
		}
		// Path is locked on edit (identity); only validate the grammar on create.
		if (!isEdit && pathError) {
			error = pathError;
			return;
		}
		loading = true;
		error = null;
		// Create at the shared platform tier when the admin opted in (create only).
		// The backend re-checks the admin flag and forces PLATFORM_SCOPE_ID.
		const scopeKind = !isEdit && isPlatformAdmin && createAsPlatform ? 'platform' : undefined;
		try {
			if (kind === 'cluster') {
				const config = buildClusterConfig();
				if (isEdit && editing) {
					await updateResource(editing.id, {
						display_name: displayName || null,
						config
					});
				} else {
					await createResource({
						path,
						resource_type: 'datacenter',
						display_name: displayName || null,
						config,
						scope_kind: scopeKind
					});
				}
			} else if (kind === 'limit') {
				// The one editable `capacity` axis: the seeded count. Re-send the
				// preset so update re-expands the locked axes + re-seeds the net.
				const n = parseInt(count, 10);
				if (!Number.isFinite(n) || n < 1) {
					error = 'Enter a count of at least 1.';
					loading = false;
					return;
				}
				const config = { preset: 'limit', capacity_amount: n };
				if (isEdit && editing) {
					await updateResource(editing.id, { display_name: displayName || null, config });
				} else {
					await createResource({
						path,
						resource_type: 'capacity',
						display_name: displayName || null,
						config,
						scope_kind: scopeKind
					});
				}
			} else {
				// runner_group / worker / (null-backend) — no editable config field.
				// On edit only the display name changes; never re-send the axes (a
				// name-only update leaves a backend-less capacity untouched).
				if (isEdit && editing) {
					// The display name is the only mutable field here; the backend
					// rejects an empty update, so require one.
					if (!displayName) {
						error = 'Enter a display name to update.';
						loading = false;
						return;
					}
					await updateResource(editing.id, {
						display_name: displayName,
						config: null
					});
				} else {
					await createResource({
						path,
						resource_type: 'capacity',
						display_name: displayName || null,
						config: { preset: activeKind.preset },
						scope_kind: scopeKind
					});
				}
			}
			onsaved();
		} catch (e) {
			error = e instanceof Error ? e.message : isEdit ? 'Update failed' : 'Create failed';
		} finally {
			loading = false;
		}
	}
</script>

<Sheet.Root
	{open}
	onOpenChange={(o: boolean) => {
		if (!o) open = false;
	}}
>
	<SheetContent class="w-[520px] overflow-y-auto sm:max-w-[520px]">
		<div class="space-y-5 p-2" data-testid="new-capacity-modal">
			<div class="space-y-1">
				<SheetTitle class="text-lg font-semibold">
					{isEdit ? 'Edit pool' : 'New pool'}
				</SheetTitle>
				<SheetDescription class="text-sm text-muted-foreground">
					{isEdit
						? 'Update this pool. Its kind and name are fixed; change the editable fields below.'
						: 'Pick the kind of pool to add.'}
				</SheetDescription>
				{#if editIsPlatform}
					<span
						class="inline-flex w-fit items-center gap-1 rounded-md bg-sky-100 px-2 py-0.5 text-xs font-medium text-sky-800"
						title="Platform tier — shared across all workspaces, managed by platform admins"
						data-testid="new-capacity-platform-badge"
					>
						<Globe class="size-3" /> Platform (shared)
					</span>
				{/if}
			</div>

			{#if error}
				<div
					class="rounded-md border border-destructive/30 bg-destructive/10 px-3 py-2 text-sm text-destructive"
				>
					{error}
				</div>
			{/if}

			<!-- Kind switcher — locked on edit (the kind is fixed by the backend). -->
			<div class="grid grid-cols-2 gap-2" data-testid="capacity-kind-switcher">
				{#each KINDS as k (k.kind)}
					{@const Icon = KIND_ICON[k.kind]}
					<button
						type="button"
						disabled={isEdit}
						onclick={() => !isEdit && (kind = k.kind)}
						class="flex items-start gap-2 rounded-lg border p-3 text-left transition-colors
							{kind === k.kind
							? 'border-primary/60 bg-accent/60'
							: 'border-border bg-card hover:bg-accent/40'}
							{isEdit && kind !== k.kind ? 'opacity-40' : ''}
							{isEdit ? 'cursor-default' : ''}"
						data-testid="capacity-kind-{k.kind}"
					>
						<Icon class="mt-0.5 size-4 shrink-0 text-muted-foreground" />
						<span class="min-w-0">
							<span class="block text-sm font-medium text-foreground">{k.label}</span>
							<span class="block text-sm text-muted-foreground">{k.hint}</span>
						</span>
					</button>
				{/each}
			</div>

			<!-- Platform tier — a shared pool curated by platform admins, visible
				 read-only to every workspace. Offered on create to admins only; the
				 backend independently 403s a non-admin platform mint. -->
			{#if !isEdit && isPlatformAdmin}
				<label
					class="flex items-start gap-2.5 rounded-md border border-sky-200 bg-sky-50/60 p-3 text-sm"
					data-testid="new-capacity-platform-toggle"
				>
					<input
						type="checkbox"
						checked={createAsPlatform}
						onchange={(e) => (createAsPlatform = (e.currentTarget as HTMLInputElement).checked)}
						class="mt-0.5 size-4"
					/>
					<span>
						<span class="flex items-center gap-1.5 font-medium text-foreground">
							<Globe class="size-3.5" /> Platform (shared)
						</span>
						<span class="text-muted-foreground">
							Create at the global platform tier — every workspace can run against it,
							but only platform admins curate it. Otherwise it's scoped to your workspace.
						</span>
					</span>
				</label>
			{/if}

			<!-- Shared name + display fields -->
			<div class="space-y-4">
				<FormField
					label="Name"
					description={isEdit
						? 'The binding alias — locked once steps and runners reference it.'
						: 'Snake_case identifier. The alias steps + runners bind to.'}
				>
					<Input
						type="text"
						value={path}
						placeholder={kind === 'cluster' ? 'hpc_cluster' : 'gpu_lab'}
						oninput={(e) => (path = (e.currentTarget as HTMLInputElement).value)}
						disabled={isEdit}
						aria-invalid={pathError ? 'true' : undefined}
						class="font-mono text-sm"
						data-testid="new-capacity-path"
					/>
					{#if pathError && !isEdit}
						<p class="mt-1 text-sm text-destructive">{pathError}</p>
					{/if}
				</FormField>

				<FormField label="Display name">
					<Input
						type="text"
						value={displayName}
						placeholder={path || 'Optional label'}
						oninput={(e) => (displayName = (e.currentTarget as HTMLInputElement).value)}
						class="text-sm"
					/>
				</FormField>

				{#if kind === 'limit'}
					<FormField
						label="Count"
						description="How many concurrent tokens this limit seeds (the fixed `N`)."
					>
						<Input
							type="number"
							min="1"
							value={count}
							oninput={(e) => (count = (e.currentTarget as HTMLInputElement).value)}
							class="text-sm"
							data-testid="new-capacity-count"
						/>
					</FormField>
				{:else if kind === 'cluster'}
					{#if datacenterDescriptor}
						<SchemaFields
							descriptor={datacenterDescriptor}
							bind:fieldValues
							bind:discriminator
							secretPlaceholder={isEdit ? '(leave blank to keep current)' : undefined}
						/>
					{:else}
						<p class="text-sm text-muted-foreground">Loading cluster schema…</p>
					{/if}
				{/if}
			</div>

			<!-- Footer -->
			<div class="flex items-center justify-end gap-2 border-t border-border pt-3">
				<SheetClose>
					<Button type="button" variant="ghost" size="sm">Cancel</Button>
				</SheetClose>
				<Button size="sm" onclick={submit} disabled={loading} data-testid="new-capacity-submit">
					{loading
						? isEdit
							? 'Saving…'
							: 'Creating…'
						: isEdit
							? 'Save changes'
							: 'Create pool'}
				</Button>
			</div>
		</div>
	</SheetContent>
</Sheet.Root>
