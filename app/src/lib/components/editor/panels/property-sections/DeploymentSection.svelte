<script lang="ts">
	import type { AutomatedStepNodeData } from '$lib/types/editor';
	import { untrack } from 'svelte';
	import * as Select from '$lib/components/ui/select';
	import { Checkbox } from '$lib/components/ui/checkbox';
	import { Textarea } from '$lib/components/ui/textarea';
	import { Input } from '$lib/components/ui/input';
	import { FormField } from '$lib/components/ui/form-field';
	import { listResources, type ResourceSummary } from '$lib/api/resources';
	import JobTemplatePicker, {
		type JobTemplateRef
	} from './shared/JobTemplatePicker.svelte';
	import TemplateParameterForm from './TemplateParameterForm.svelte';

	// The editor-side shape of `DeploymentModel` (executor{capacity?} | scheduled{…}).
	// Shared by the AutomatedStep and Agent panels — both carry the identical
	// `deploymentModel` field derived from the same OpenAPI schema.
	type DeploymentModelValue = NonNullable<AutomatedStepNodeData['deploymentModel']>;

	// Phase 3 B-model extension: the Scheduled variant carries an optional
	// `jobTemplateRef` + `jobTemplateParams` that the schema hasn't been
	// regenerated for yet. We add them locally; the Yjs doc is freeform JSON
	// so they survive round-trips without schema changes. At publish time the
	// mekhan compiler reads `jobTemplateRef` and stamps `jobTemplate` with the
	// resolved slug.
	type ScheduledExtended = {
		mode: 'scheduled';
		jobTemplate: string;
		scheduler?: string | null;
		resources?: null | unknown;
		/** Phase 3 B-model: pointer to a control-plane job-template. When set,
		 *  publish resolves the slug into `jobTemplate`. */
		jobTemplateRef?: JobTemplateRef | null;
		/** Phase 3 B-model: parameter values for the picked template. */
		jobTemplateParams?: Record<string, unknown>;
	};

	type ExtendedDeploymentModel =
		| { mode: 'executor'; capacity?: null | unknown }
		| ScheduledExtended;

	type Props = {
		value: DeploymentModelValue | undefined;
		/** Whether the Scheduled (Nomad/Slurm) toggle is offered. Engine-effect
		 * backends are inline-only; pass `false` to hide it. */
		schedulable?: boolean;
		readonly?: boolean;
		onchange: (deploymentModel: DeploymentModelValue) => void;
	};

	let { value, schedulable = true, readonly = false, onchange }: Props = $props();

	// Cast to our extended type so we can read/write the new optional fields
	// without fighting the narrower schema type.
	const ext = $derived(value as ExtendedDeploymentModel | undefined);

	// Deployment model — executor (our executor daemon pool over the NATS work
	// queue) vs scheduled (external cluster — Nomad/Slurm). Optional-chained so
	// legacy templates (no field) render as executor; Rust `#[serde(default)]`
	// covers the wire side.
	const deploymentMode = $derived(value?.mode ?? 'executor');
	const jobTemplate = $derived(value?.mode === 'scheduled' ? value.jobTemplate : '');
	const allowScheduled = $derived(schedulable);

	const scheduler = $derived(value?.mode === 'scheduled' ? (value.scheduler ?? '') : '');

	// Phase 3 B-model fields
	const jobTemplateRef = $derived(
		ext?.mode === 'scheduled' ? (ext.jobTemplateRef ?? null) : null
	);
	const jobTemplateParams = $derived(
		ext?.mode === 'scheduled' ? (ext.jobTemplateParams ?? {}) : {}
	);

	// Flavor hint for the picker: derive from the selected scheduler resource's
	// flavor when it's available (future; for now pass null → show all templates).
	const pickerFlavor = $derived<string | null>(null);

	// Whether to show the free-text job template override (legacy path or manual).
	let showManualTemplate = $state(false);
	$effect(() => {
		// Auto-expand the manual field when there's a pre-existing free-text value
		// and no structured ref (so legacy templates don't silently lose their value).
		if (ext?.mode === 'scheduled' && ext.jobTemplate && !ext.jobTemplateRef) {
			untrack(() => { showManualTemplate = true; });
		}
	});

	// Switching to scheduled drops any executor capacity binding (capacity is
	// Executor-only — a datacenter cluster is bound under Scheduled instead).
	// Defaults to env-global scheduler.
	function setDeploymentMode(mode: string) {
		if (mode === 'scheduled') {
			const dm: ScheduledExtended = { mode: 'scheduled', jobTemplate: '' };
			onchange(dm as unknown as DeploymentModelValue);
		} else {
			onchange({ mode: 'executor' } as DeploymentModelValue);
		}
	}

	function setJobTemplate(v: string) {
		if (value?.mode !== 'scheduled') return;
		onchange({ ...value, jobTemplate: v } as DeploymentModelValue);
	}

	function setScheduler(alias: string) {
		if (value?.mode !== 'scheduled') return;
		const dm = { ...value } as ScheduledExtended;
		if (alias) dm.scheduler = alias;
		else delete dm.scheduler;
		onchange(dm as unknown as DeploymentModelValue);
	}

	function setJobTemplateRef(ref: JobTemplateRef | null) {
		if (value?.mode !== 'scheduled') return;
		const dm = { ...value } as ScheduledExtended;
		dm.jobTemplateRef = ref ?? undefined;
		if (!ref) {
			// Clear params when the template is deselected.
			delete dm.jobTemplateParams;
		}
		onchange(dm as unknown as DeploymentModelValue);
	}

	function setJobTemplateParams(params: Record<string, unknown>) {
		if (value?.mode !== 'scheduled') return;
		const dm = { ...value } as ScheduledExtended;
		dm.jobTemplateParams = Object.keys(params).length > 0 ? params : undefined;
		onchange(dm as unknown as DeploymentModelValue);
	}

	// Executor capacity admission. The binding lives under
	// `deploymentModel.Executor.capacity` (post-R3 consolidation); presence =
	// "claim a unit from this concurrency_limit / runner_group". `alias` is
	// REQUIRED — a limited step names a concurrency_limit or runner_group
	// resource (no well-known-global fallback).
	const poolAlias = $derived(value?.mode === 'executor' ? ((value as { mode: 'executor'; capacity?: null | { alias: string } }).capacity?.alias ?? '') : '');
	const requiresPool = $derived(value?.mode === 'executor' && (value as { mode: 'executor'; capacity?: null | unknown }).capacity != null);
	// Capacity is intrinsically executor-only now (it lives under
	// Executor.capacity), so the control is simply hidden while scheduled.
	const poolControlsVisible = $derived(deploymentMode === 'executor');

	function setRequiresPool(on: boolean) {
		onchange(on ? { mode: 'executor', capacity: { alias: poolAlias } } as DeploymentModelValue : { mode: 'executor' } as DeploymentModelValue);
	}

	function setPoolAlias(alias: string) {
		if (value?.mode !== 'executor') return;
		// Preserve any existing request params when re-pointing the alias.
		const prev = value as { mode: 'executor'; capacity?: null | { alias: string; request?: unknown } };
		const prevRequest = prev.capacity?.request;
		onchange({
			mode: 'executor',
			capacity: { alias, ...(prevRequest !== undefined ? { request: prevRequest } : {}) }
		} as DeploymentModelValue);
	}

	// ── Optional raw-JSON `request` params (v1: a textarea, not a schema form).
	// Bound to Executor.capacity.request. Kept as text locally so
	// invalid JSON mid-typing doesn't clobber the model; committed on valid parse.
	const requestValue = $derived(value?.mode === 'executor' ? (value as { mode: 'executor'; capacity?: null | { alias: string; request?: unknown } }).capacity?.request : undefined);
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
			const extDm = dm as { mode: 'executor'; capacity?: null | { alias: string; request?: unknown } };
			if (extDm.capacity == null) return; // no capacity → nothing to attach request to
			const capacity: { alias: string; request?: unknown } = { alias: extDm.capacity.alias };
			if (parsed !== undefined) capacity.request = parsed;
			onchange({ mode: 'executor', capacity } as DeploymentModelValue);
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
			// A capacity binding can name either kind — list both.
			Promise.all([
				listResources({ resource_type: 'concurrency_limit', perPage: 200 }),
				listResources({ resource_type: 'runner_group', perPage: 200 })
			])
				.then(([cl, rg]) => (poolResources = [...cl.items, ...rg.items]))
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
		if (!poolAlias) return 'Select a capacity (concurrency limit or runner group)…';
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
					: 'Executor (workers)'}
			</Select.Trigger>
			<Select.Content>
				<Select.Item value="executor" label="Executor (workers)" />
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

			<!-- Phase 3 B-model: structured job-template picker -->
			<JobTemplatePicker
				flavor={pickerFlavor}
				selected={jobTemplateRef}
				onChange={setJobTemplateRef}
				label="Job template"
				{readonly}
				testId="select-job-template-ref"
			/>

			<!-- Parameter form: shown when a template with declared params is picked -->
			<TemplateParameterForm
				templateRef={jobTemplateRef}
				values={jobTemplateParams}
				onchange={setJobTemplateParams}
				{readonly}
			/>

			<!-- Manual override toggle: exposes the legacy free-text job_template field -->
			<div class="space-y-1 pt-1">
				<label class="flex items-center gap-1.5 text-sm text-muted-foreground">
					<Checkbox
						checked={showManualTemplate}
						disabled={readonly}
						onCheckedChange={(v) => (showManualTemplate = v === true)}
						data-testid="toggle-manual-job-template"
					/>
					Override job template name manually
				</label>
				{#if showManualTemplate}
					<FormField label="Job template name (manual)" for="deployment-job-template-manual">
						<Input
							id="deployment-job-template-manual"
							type="text"
							class="font-mono text-sm"
							value={jobTemplate}
							disabled={readonly}
							placeholder="e.g. petri-mumax3-worker"
							data-testid="input-job-template"
							oninput={(e) => setJobTemplate((e.currentTarget as HTMLInputElement).value)}
						/>
					</FormField>
					<p class="text-sm italic text-muted-foreground">
						Overrides the picker above. Use when the job name is pre-registered on the cluster and
						does not yet have a control-plane template.
					</p>
				{/if}
			</div>

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
				Claim a concurrency limit
			</label>
			{#if requiresPool}
				<FormField label="Capacity resource" for="pool-alias">
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
						Select a concurrency limit — a limited step must name a resource.
					</p>
				{:else if poolResources.length === 0 && poolResourcesLoaded}
					<p class="text-sm italic text-muted-foreground">
						No <code class="font-mono">concurrency_limit</code> or
						<code class="font-mono">runner_group</code> resources in this workspace. Add one under
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
					Holds a unit from the named concurrency_limit (or runner_group) resource for the step's
					duration; queues when the limit is reached. The granted lease is readable in the body as
					<code>lease.unit_id</code>.
				</p>
			{/if}
		</div>
	{/if}
</div>
