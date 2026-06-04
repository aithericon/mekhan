<script lang="ts">
	// Control-Plane "New capacity" modal. A kind switcher across the four ways an
	// operator adds dispatch capacity:
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
	import SchemaFields, {
		deriveFieldSpecs,
		type FieldSpec
	} from '$lib/components/resources/SchemaFields.svelte';
	import {
		createResource,
		listResourceTypes,
		type ResourceTypeInfo
	} from '$lib/api/resources';

	type Props = {
		open: boolean;
		/** Optional pre-loaded type list (the page already fetched it). */
		types?: ResourceTypeInfo[];
		/** Called after a successful create (parent closes + refreshes). */
		oncreated: () => void;
	};

	let { open = $bindable(), types: typesProp = [], oncreated }: Props = $props();

	// ── Kind switcher ───────────────────────────────────────────────────────────
	type Kind = 'runner_group' | 'limit' | 'worker' | 'cluster';
	const KINDS: { kind: Kind; label: string; preset?: string; hint: string }[] = [
		{
			kind: 'runner_group',
			label: 'Runner group',
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
			hint: 'A pull queue fungible workers drain. No held claim.'
		},
		{
			kind: 'cluster',
			label: 'Cluster',
			hint: 'A scheduler datacenter (Slurm / Nomad / HTTP) leasing allocations.'
		}
	];
	const KIND_ICON: Record<Kind, typeof Server> = {
		runner_group: Server,
		limit: KeyRound,
		worker: Cpu,
		cluster: Boxes
	};

	let kind = $state<Kind>('runner_group');
	const activeKind = $derived(KINDS.find((k) => k.kind === kind)!);

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
		error = null;
		// Reset the form each open.
		kind = 'runner_group';
		path = '';
		displayName = '';
		count = '1';
		fieldValues = {};
		(async () => {
			if (types.length === 0) {
				types = typesProp.length > 0 ? typesProp : await listResourceTypes().catch((e) => {
					error = e instanceof Error ? e.message : 'Failed to load types';
					return [];
				});
			}
		})();
	});

	// ── Submit ────────────────────────────────────────────────────────────────────
	function buildClusterConfig(): Record<string, unknown> {
		const out: Record<string, unknown> = {};
		for (const spec of clusterFieldSpecs) {
			const raw = fieldValues[spec.name] ?? '';
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
		if (pathError) {
			error = pathError;
			return;
		}
		loading = true;
		error = null;
		try {
			if (kind === 'cluster') {
				await createResource({
					path,
					resource_type: 'datacenter',
					display_name: displayName || null,
					config: buildClusterConfig()
				});
			} else {
				// runner_group / limit / worker → a `capacity` from a named preset.
				const config: Record<string, unknown> = { preset: activeKind.preset };
				if (kind === 'limit') {
					const n = parseInt(count, 10);
					if (!Number.isFinite(n) || n < 1) {
						error = 'Enter a count of at least 1.';
						loading = false;
						return;
					}
					config.capacity_amount = n;
				}
				await createResource({
					path,
					resource_type: 'capacity',
					display_name: displayName || null,
					config
				});
			}
			oncreated();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Create failed';
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
				<SheetTitle class="text-lg font-semibold">New capacity</SheetTitle>
				<SheetDescription class="text-sm text-muted-foreground">
					Pick the kind of dispatch capacity to add.
				</SheetDescription>
			</div>

			{#if error}
				<div
					class="rounded-md border border-destructive/30 bg-destructive/10 px-3 py-2 text-sm text-destructive"
				>
					{error}
				</div>
			{/if}

			<!-- Kind switcher -->
			<div class="grid grid-cols-2 gap-2" data-testid="capacity-kind-switcher">
				{#each KINDS as k (k.kind)}
					{@const Icon = KIND_ICON[k.kind]}
					<button
						type="button"
						onclick={() => (kind = k.kind)}
						class="flex items-start gap-2 rounded-lg border p-3 text-left transition-colors
							{kind === k.kind
							? 'border-primary/60 bg-accent/60'
							: 'border-border bg-card hover:bg-accent/40'}"
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

			<!-- Shared name + display fields -->
			<div class="space-y-4">
				<FormField
					label="Name"
					description="Snake_case identifier. The alias steps + runners bind to."
				>
					<Input
						type="text"
						value={path}
						placeholder={kind === 'cluster' ? 'hpc_cluster' : 'gpu_lab'}
						oninput={(e) => (path = (e.currentTarget as HTMLInputElement).value)}
						aria-invalid={pathError ? 'true' : undefined}
						class="font-mono text-sm"
						data-testid="new-capacity-path"
					/>
					{#if pathError}
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
						<SchemaFields descriptor={datacenterDescriptor} bind:fieldValues bind:discriminator />
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
					{loading ? 'Creating…' : 'Create capacity'}
				</Button>
			</div>
		</div>
	</SheetContent>
</Sheet.Root>
