<script lang="ts">
	import type { LeaseScopeNodeData } from '$lib/types/editor';
	import type { components } from '$lib/api/schema';
	import { untrack } from 'svelte';
	import * as Select from '$lib/components/ui/select';
	import { Textarea } from '$lib/components/ui/textarea';
	import { FormField } from '$lib/components/ui/form-field';
	import { listResources, type ResourceSummary } from '$lib/api/resources';
	import PlacementRequirementsSection from './PlacementRequirementsSection.svelte';

	type Requirements = components['schemas']['Requirements'];

	type Props = {
		data: LeaseScopeNodeData;
		readonly?: boolean;
		onchange: (data: LeaseScopeNodeData) => void;
	};

	let { data, readonly = false, onchange }: Props = $props();

	// The held lease binding. `pool` is the capacity-provider alias the scope
	// holds ONE unit against — a `datacenter` (a leased cluster allocation) OR a
	// presence `capacity` (a single lab runner). REQUIRED (a lease-less scope is a
	// pointless empty container — the compiler's validate_lease_scope rejects an
	// empty alias). `request` is the optional claim-schema-shaped params blob (a
	// datacenter's alloc shape); `requirements` is the optional presence
	// cap-match (the scope picks WHICH runner to hold).
	const pool = $derived((data.lease?.pool ?? '').trim());
	const requestValue = $derived(data.lease?.request);

	function setPool(alias: string) {
		const prevRequest = data.lease?.request;
		onchange({
			...data,
			lease: {
				pool: alias,
				...(prevRequest !== undefined ? { request: prevRequest } : {})
			}
		});
	}

	function setRequirements(req: Requirements | undefined) {
		onchange({ ...data, requirements: req ?? null });
	}

	// ── Optional raw-JSON `request` params (v1: a textarea, not a schema form).
	// Kept as text locally so invalid JSON mid-typing doesn't clobber the model;
	// committed on valid parse. Mirrors DeploymentSection's request handling.
	let requestText = $state('');
	let requestError = $state<string | null>(null);
	$effect(() => {
		const v = requestValue;
		untrack(() => {
			requestText = v === undefined ? '' : JSON.stringify(v, null, 2);
			requestError = null;
		});
	});

	function commitRequest(text: string) {
		requestText = text;
		const trimmed = text.trim();
		let parsed: unknown;
		if (trimmed === '') {
			parsed = undefined;
		} else {
			try {
				parsed = JSON.parse(trimmed);
			} catch {
				requestError = 'Invalid JSON — not saved';
				return;
			}
		}
		requestError = null;
		const lease: { pool: string; request?: unknown } = {
			pool: data.lease?.pool ?? ''
		};
		if (parsed !== undefined) lease.request = parsed;
		onchange({ ...data, lease });
	}

	// ── Capacity-provider picker. A LeaseScope binds to a `datacenter` (Scheduler
	// backend) OR a presence `capacity` (Presence backend); both park a held lease
	// the body inherits. Load both kinds; a `capacity` row carries `public_config`
	// so we can keep only the presence ones (a `tokens`/seeded limit has no held
	// namespace and the compiler rejects it).
	type ProviderKind = 'datacenter' | 'presence';
	type Provider = { path: string; display_name: string; id: string; kind: ProviderKind };
	let providers = $state<Provider[]>([]);
	let providersLoaded = $state(false);
	$effect(() => {
		if (providersLoaded) return;
		providersLoaded = true;
		Promise.all([
			listResources({ resource_type: 'datacenter', perPage: 200 }).catch(
				() => ({ items: [] as ResourceSummary[] })
			),
			listResources({ resource_type: 'capacity', perPage: 200 }).catch(
				() => ({ items: [] as ResourceSummary[] })
			)
		]).then(([dcs, caps]) => {
			const dcProviders: Provider[] = dcs.items.map((r) => ({
				path: r.path,
				display_name: r.display_name,
				id: r.id,
				kind: 'datacenter'
			}));
			// Keep only presence capacities (liveness === "presence"); a seeded
			// (concurrency-limit) or competing-consumer (worker) capacity cannot
			// back a held lease.
			const presenceProviders: Provider[] = caps.items
				.filter((r) => {
					const cfg = r.public_config as { liveness?: string } | null | undefined;
					return cfg?.liveness === 'presence';
				})
				.map((r) => ({
					path: r.path,
					display_name: r.display_name,
					id: r.id,
					kind: 'presence'
				}));
			providers = [...dcProviders, ...presenceProviders];
		});
	});

	const selectedProvider = $derived(providers.find((p) => p.path === pool));
	// While the list is still loading we can't classify the alias; default to
	// showing the datacenter `request` field (the pre-existing behavior) until the
	// resource kind resolves.
	const isPresencePool = $derived(selectedProvider?.kind === 'presence');

	function poolLabel(): string {
		if (!pool) return 'Select a capacity provider…';
		const found = providers.find((r) => r.path === pool);
		const suffix = found?.kind === 'presence' ? ' (runner pool)' : ' (datacenter)';
		return found ? `${found.path} — ${found.display_name}${suffix}` : pool;
	}
</script>

<!--
	A LeaseScope holds ONE unit of capacity for its whole body: every step placed
	inside the container runs on the held unit (acquire on enter / release on
	exit), with no per-step flag. Compose a Loop inside for warm iteration, or
	sequential steps for a warm pipeline. The single binding below names the
	capacity provider — a datacenter (cluster alloc) or a presence runner pool.
-->
<div class="space-y-2 pt-3 border-t border-border/40">
	<span class="text-sm font-medium text-muted-foreground">Lease binding</span>

	<FormField label="Capacity provider" for="lease-scope-pool">
		<Select.Root
			type="single"
			value={pool}
			onValueChange={(v) => setPool(v ?? '')}
			disabled={readonly}
		>
			<Select.Trigger disabled={readonly} data-testid="select-lease-scope-pool">
				<span class="truncate text-sm">{poolLabel()}</span>
			</Select.Trigger>
			<Select.Content>
				{#each providers as r (r.id)}
					<Select.Item
						value={r.path}
						label={`${r.path} — ${r.display_name}${r.kind === 'presence' ? ' (runner pool)' : ' (datacenter)'}`}
					/>
				{/each}
			</Select.Content>
		</Select.Root>
	</FormField>
	{#if !pool}
		<p class="text-sm text-destructive">
			A lease scope must name a <code class="font-mono">datacenter</code> or a presence
			<code class="font-mono">capacity</code> — the held unit comes from it.
		</p>
	{:else if providers.length === 0 && providersLoaded}
		<p class="text-sm italic text-muted-foreground">
			No leasable providers in this workspace. Add a <code class="font-mono">datacenter</code> or
			an instrument <code class="font-mono">capacity</code> under
			<code class="font-mono">/resources</code>.
		</p>
	{/if}
	{#if pool}
		<p class="text-xs italic text-muted-foreground">
			Sets the <strong>default binding</strong> for this template's home workspace. Forks and other
			workspaces bind their own provider under <em>Configure resources</em>, and each run can override
			it in the launch dialog.
		</p>
	{/if}

	{#if isPresencePool}
		<!-- Presence lease: cap-match WHICH runner to hold. -->
		<PlacementRequirementsSection
			requirements={data.requirements}
			{readonly}
			onchange={setRequirements}
		/>
		<p class="text-sm text-muted-foreground">
			The held runner is chosen by these constraints (matched against advertised capabilities). Every
			step inside this scope runs on that same held runner automatically — its planning scene stays
			warm across the whole body.
		</p>
	{:else}
		<!-- Datacenter lease: optional claim-schema-shaped request params. -->
		<FormField label="Request (optional)" for="lease-scope-request">
			<Textarea
				id="lease-scope-request"
				class="font-mono text-sm"
				rows={3}
				value={requestText}
				disabled={readonly}
				placeholder={'{ "gpu_count": 1, "gpu_type": "a100" }'}
				oninput={(e) => commitRequest((e.currentTarget as HTMLTextAreaElement).value)}
				data-testid="textarea-lease-scope-request"
			/>
		</FormField>
		{#if requestError}
			<p class="text-sm text-destructive">{requestError}</p>
		{/if}
		<p class="text-sm text-muted-foreground">
			Lease params validated against the datacenter kind’s claim schema; the granted lease is
			readable in the body as <code class="font-mono">lease.alloc_id</code> /
			<code class="font-mono">lease.node</code> /
			<code class="font-mono">lease.scheduler.&lt;field&gt;</code> (the fields depend on the
			resolved scheduler flavor). Steps inside this scope run on the held allocation automatically.
		</p>
	{/if}
</div>
