<script lang="ts">
	import type { LeaseScopeNodeData } from '$lib/types/editor';
	import { untrack } from 'svelte';
	import * as Select from '$lib/components/ui/select';
	import { Textarea } from '$lib/components/ui/textarea';
	import { FormField } from '$lib/components/ui/form-field';
	import { listResources, type ResourceSummary } from '$lib/api/resources';

	type Props = {
		data: LeaseScopeNodeData;
		readonly?: boolean;
		onchange: (data: LeaseScopeNodeData) => void;
	};

	let { data, readonly = false, onchange }: Props = $props();

	// The held lease binding. `scheduler` is the datacenter resource alias the
	// scope holds an allocation against; REQUIRED (a lease-less scope is a
	// pointless empty container — the compiler's validate_lease_scope rejects an
	// empty alias). `request` is the optional claim-schema-shaped params blob.
	const scheduler = $derived((data.lease?.scheduler ?? '').trim());
	const requestValue = $derived(data.lease?.request);

	function setScheduler(alias: string) {
		const prevRequest = data.lease?.request;
		onchange({
			...data,
			lease: {
				scheduler: alias,
				...(prevRequest !== undefined ? { request: prevRequest } : {})
			}
		});
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
		const lease: { scheduler: string; request?: unknown } = {
			scheduler: data.lease?.scheduler ?? ''
		};
		if (parsed !== undefined) lease.request = parsed;
		onchange({ ...data, lease });
	}

	// ── Datacenter resource picker. A LeaseScope binds to a `datacenter`
	// resource exactly like the Scheduled-lease path (resolve_binding(...,
	// "datacenter", ...)); load the workspace's datacenter resources once.
	let schedulerResources = $state<ResourceSummary[]>([]);
	let schedulerResourcesLoaded = $state(false);
	$effect(() => {
		if (schedulerResourcesLoaded) return;
		schedulerResourcesLoaded = true;
		listResources({ resource_type: 'datacenter', perPage: 200 })
			.then((p) => (schedulerResources = p.items))
			.catch(() => {
				/* leave empty — picker shows the empty hint */
			});
	});

	function schedulerLabel(): string {
		if (!scheduler) return 'Select a datacenter resource…';
		const found = schedulerResources.find((r) => r.path === scheduler);
		return found ? `${found.path} — ${found.display_name}` : scheduler;
	}
</script>

<!--
	A LeaseScope holds ONE allocation for its whole body: every step placed
	inside the container runs on the held lease (acquire on enter / release on
	exit), with no per-step flag. Compose a Loop inside for warm iteration, or
	sequential steps for a warm pipeline. The single binding below names the
	datacenter the allocation comes from.
-->
<div class="space-y-2 pt-3 border-t border-border/40">
	<span class="text-sm font-medium text-muted-foreground">Lease binding</span>

	<FormField label="Datacenter resource" for="lease-scope-scheduler">
		<Select.Root
			type="single"
			value={scheduler}
			onValueChange={(v) => setScheduler(v ?? '')}
			disabled={readonly}
		>
			<Select.Trigger disabled={readonly} data-testid="select-lease-scope-scheduler">
				<span class="truncate text-sm">{schedulerLabel()}</span>
			</Select.Trigger>
			<Select.Content>
				{#each schedulerResources as r (r.id)}
					<Select.Item value={r.path} label={`${r.path} — ${r.display_name}`} />
				{/each}
			</Select.Content>
		</Select.Root>
	</FormField>
	{#if !scheduler}
		<p class="text-sm text-destructive">
			A lease scope must name a <code class="font-mono">datacenter</code> resource — the held
			allocation comes from it.
		</p>
	{:else if schedulerResources.length === 0 && schedulerResourcesLoaded}
		<p class="text-sm italic text-muted-foreground">
			No <code class="font-mono">datacenter</code> resources in this workspace. Add one under
			<code class="font-mono">/resources</code> to lease external cluster allocations.
		</p>
	{/if}

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
		readable in the body as <code class="font-mono">lease.node</code> /
		<code class="font-mono">lease.gpu_uuid</code> / <code class="font-mono">lease.alloc_id</code>.
		Steps inside this scope run on the held allocation automatically.
	</p>
</div>
