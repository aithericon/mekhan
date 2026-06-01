<script lang="ts">
	import type { AutomatedStepNodeData } from '$lib/types/editor';
	import { untrack } from 'svelte';
	import * as Select from '$lib/components/ui/select';
	import { Checkbox } from '$lib/components/ui/checkbox';
	import { Textarea } from '$lib/components/ui/textarea';
	import { FormField } from '$lib/components/ui/form-field';
	import { listResources, type ResourceSummary } from '$lib/api/resources';
	import { GPU_JOB_TEMPLATES } from '$lib/editor/deployment-targets';

	// The editor-side shape of `DeploymentModel` (executor{pool?} | scheduled{…}).
	// Shared by the AutomatedStep and Agent panels — both carry the identical
	// `deploymentModel` field derived from the same OpenAPI schema.
	type DeploymentModelValue = NonNullable<AutomatedStepNodeData['deploymentModel']>;

	type Props = {
		value: DeploymentModelValue | undefined;
		/** Whether the Scheduled (Nomad/Slurm) toggle is offered. Engine-effect
		 * backends are inline-only; pass `false` to hide it. */
		schedulable?: boolean;
		readonly?: boolean;
		onchange: (deploymentModel: DeploymentModelValue) => void;
	};

	let { value, schedulable = true, readonly = false, onchange }: Props = $props();

	// Deployment model — executor (our executor daemon pool over the NATS work
	// queue) vs scheduled (external cluster — Nomad/Slurm). Optional-chained so
	// legacy templates (no field) render as executor; Rust `#[serde(default)]`
	// covers the wire side.
	const deploymentMode = $derived(value?.mode ?? 'executor');
	const jobTemplate = $derived(value?.mode === 'scheduled' ? value.jobTemplate : '');
	const allowScheduled = $derived(schedulable);

	const scheduler = $derived(value?.mode === 'scheduled' ? (value.scheduler ?? '') : '');

	// Switching to scheduled drops any executor pool (pool is Executor-only — a
	// datacenter cluster is bound under Scheduled instead). Defaults to
	// env-global scheduler.
	function setDeploymentMode(mode: string) {
		onchange(
			mode === 'scheduled'
				? { mode: 'scheduled', jobTemplate: jobTemplate || GPU_JOB_TEMPLATES[0].value }
				: { mode: 'executor' }
		);
	}

	function setJobTemplate(v: string) {
		if (value?.mode !== 'scheduled') return;
		onchange({ ...value, jobTemplate: v });
	}

	function setScheduler(alias: string) {
		if (value?.mode !== 'scheduled') return;
		const dm = { ...value };
		if (alias) dm.scheduler = alias;
		else delete dm.scheduler;
		onchange(dm);
	}

	// Executor-pool token admission. The binding lives under
	// `deploymentModel.Executor.pool` (post-R3 consolidation); presence = "claim
	// a unit from this token_pool". `alias` is REQUIRED — a pooled step names a
	// token_pool resource (no well-known-global fallback).
	const poolAlias = $derived(value?.mode === 'executor' ? (value.pool?.alias ?? '') : '');
	const requiresPool = $derived(value?.mode === 'executor' && value.pool != null);
	// Pool is intrinsically executor-only now (it lives under Executor.pool), so
	// the control is simply hidden while scheduled.
	const poolControlsVisible = $derived(deploymentMode === 'executor');

	function setRequiresPool(on: boolean) {
		onchange(on ? { mode: 'executor', pool: { alias: poolAlias } } : { mode: 'executor' });
	}

	function setPoolAlias(alias: string) {
		if (value?.mode !== 'executor') return;
		// Preserve any existing request params when re-pointing the alias.
		const prevRequest = value.pool?.request;
		onchange({
			mode: 'executor',
			pool: { alias, ...(prevRequest !== undefined ? { request: prevRequest } : {}) }
		});
	}

	// ── Optional raw-JSON `request` params (v1: a textarea, not a schema form).
	// Bound to Executor.pool.request. Kept as text locally so
	// invalid JSON mid-typing doesn't clobber the model; committed on valid parse.
	const requestValue = $derived(value?.mode === 'executor' ? value.pool?.request : undefined);
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
		const dm = value;
		if (!dm) return;
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
		if (dm.mode === 'executor') {
			if (dm.pool == null) return; // no pool → nothing to attach request to
			const pool: { alias: string; request?: unknown } = { alias: dm.pool.alias };
			if (parsed !== undefined) pool.request = parsed;
			onchange({ mode: 'executor', pool });
		}
	}

	// ── Resource pickers (load workspace resources filtered by kind) ──────────
	let poolResources = $state<ResourceSummary[]>([]);
	let schedulerResources = $state<ResourceSummary[]>([]);
	let poolResourcesLoaded = $state(false);
	let schedulerResourcesLoaded = $state(false);

	$effect(() => {
		if (poolControlsVisible && requiresPool && !poolResourcesLoaded) {
			poolResourcesLoaded = true;
			listResources({ resource_type: 'token_pool', perPage: 200 })
				.then((p) => (poolResources = p.items))
				.catch(() => {
					/* leave empty — picker shows the empty hint */
				});
		}
	});
	$effect(() => {
		if (deploymentMode === 'scheduled' && !schedulerResourcesLoaded) {
			schedulerResourcesLoaded = true;
			listResources({ resource_type: 'datacenter', perPage: 200 })
				.then((p) => (schedulerResources = p.items))
				.catch(() => {
					/* leave empty — picker shows the empty hint */
				});
		}
	});

	function poolAliasLabel(): string {
		if (!poolAlias) return 'Select a token pool…';
		const found = poolResources.find((r) => r.path === poolAlias);
		return found ? `${found.path} — ${found.display_name}` : poolAlias;
	}
	function schedulerLabel(): string {
		if (!scheduler) return 'Select a datacenter resource…';
		const found = schedulerResources.find((r) => r.path === scheduler);
		return found ? `${found.path} — ${found.display_name}` : scheduler;
	}
</script>

<div class="space-y-2 pt-3 border-t border-border/40">
	<span class="text-sm font-medium text-muted-foreground">Deployment</span>
	{#if allowScheduled}
		<Select.Root
			type="single"
			value={deploymentMode}
			onValueChange={(v) => {
				if (v) setDeploymentMode(v);
			}}
			disabled={readonly}
		>
			<Select.Trigger disabled={readonly} data-testid="select-deployment-model">
				{deploymentMode === 'scheduled'
					? 'Scheduled (Nomad/Slurm cluster)'
					: 'Executor (worker pool)'}
			</Select.Trigger>
			<Select.Content>
				<Select.Item value="executor" label="Executor (worker pool)" />
				<Select.Item value="scheduled" label="Scheduled (Nomad/Slurm cluster)" />
			</Select.Content>
		</Select.Root>
		{#if deploymentMode === 'scheduled'}
			<FormField label="Scheduler (datacenter resource)" for="scheduled-scheduler">
				<Select.Root
					type="single"
					value={scheduler}
					onValueChange={(v) => setScheduler(v ?? '')}
					disabled={readonly}
				>
					<Select.Trigger disabled={readonly} data-testid="select-scheduler">
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
					Select a datacenter — a Scheduled step must lease an allocation from a specific cluster.
				</p>
			{:else if schedulerResources.length === 0 && schedulerResourcesLoaded}
				<p class="text-sm italic text-muted-foreground">
					No <code class="font-mono">datacenter</code> resources in this workspace. Add one under
					<code class="font-mono">/resources</code> to lease allocations on an external cluster.
				</p>
			{/if}

			<FormField label="Job template" for="deployment-job-template">
				<Select.Root
					type="single"
					value={jobTemplate}
					onValueChange={(v) => {
						if (v) setJobTemplate(v);
					}}
					disabled={readonly}
				>
					<Select.Trigger disabled={readonly} data-testid="select-job-template">
						{GPU_JOB_TEMPLATES.find((t) => t.value === jobTemplate)?.label ??
							(jobTemplate || 'Select a job template…')}
					</Select.Trigger>
					<Select.Content>
						{#each GPU_JOB_TEMPLATES as t (t.value)}
							<Select.Item value={t.value} label={t.label} />
						{/each}
					</Select.Content>
				</Select.Root>
			</FormField>

			<p class="text-sm text-muted-foreground">
				Leases a warm allocation from the datacenter for the step's duration; the granted lease is readable in the body as <code>lease.node</code> / <code>lease.alloc_id</code>.
			</p>
		{/if}
	{:else}
		<p class="text-sm text-muted-foreground">
			Executor only — this backend runs as an engine effect.
		</p>
	{/if}

	{#if poolControlsVisible}
		<div class="space-y-1 pt-2">
			<label class="flex items-center gap-1.5 text-sm text-foreground">
				<Checkbox
					checked={requiresPool}
					disabled={readonly}
					onCheckedChange={(v) => setRequiresPool(v === true)}
					data-testid="toggle-resource-pool"
				/>
				Claim from a token pool
			</label>
			{#if requiresPool}
				<FormField label="Pool resource" for="pool-alias">
					<Select.Root
						type="single"
						value={poolAlias}
						onValueChange={(v) => setPoolAlias(v ?? '')}
						disabled={readonly}
					>
						<Select.Trigger disabled={readonly} data-testid="select-pool-alias">
							<span class="truncate text-sm">{poolAliasLabel()}</span>
						</Select.Trigger>
						<Select.Content>
							{#each poolResources as r (r.id)}
								<Select.Item value={r.path} label={`${r.path} — ${r.display_name}`} />
							{/each}
						</Select.Content>
					</Select.Root>
				</FormField>
				{#if !poolAlias}
					<p class="text-sm text-destructive">
						Select a token pool — a pooled step must name a resource.
					</p>
				{:else if poolResources.length === 0 && poolResourcesLoaded}
					<p class="text-sm italic text-muted-foreground">
						No <code class="font-mono">token_pool</code> resources in this workspace. Add one under
						<code class="font-mono">/resources</code> first.
					</p>
				{/if}
				{#if poolAlias}
					<FormField label="Request (optional)" for="pool-request">
						<Textarea
							id="pool-request"
							class="font-mono text-sm"
							rows={2}
							value={requestText}
							disabled={readonly}
							placeholder={'{ "units": 1 }'}
							oninput={(e) => commitRequest((e.currentTarget as HTMLTextAreaElement).value)}
							data-testid="textarea-pool-request"
						/>
					</FormField>
					{#if requestError}
						<p class="text-sm text-destructive">{requestError}</p>
					{/if}
				{/if}
				<p class="text-sm text-muted-foreground">
					Holds a unit from the named token_pool resource for the step's duration; queues when the
					pool is full. The granted lease is readable in the body as <code>lease.unit_id</code>.
				</p>
			{/if}
		</div>
	{/if}
</div>
