<script lang="ts">
	import type { AutomatedStepNodeData } from '$lib/types/editor';
	import { untrack } from 'svelte';
	import * as Select from '$lib/components/ui/select';
	import { Checkbox } from '$lib/components/ui/checkbox';
	import { Textarea } from '$lib/components/ui/textarea';
	import { Input } from '$lib/components/ui/input';
	import { FormField } from '$lib/components/ui/form-field';
	import { listResources, type ResourceSummary } from '$lib/api/resources';
	import {
		resolveRunTarget,
		initialRunTarget,
		capacityTarget,
		targetsByAlias,
		type RunTarget as Target,
		type DeploymentLike
	} from '$lib/editor/deployment-run-target';
	import type { components } from '$lib/api/schema';
	import JobTemplatePicker, { type JobTemplateRef } from './shared/JobTemplatePicker.svelte';
	import TemplateParameterForm from './TemplateParameterForm.svelte';
	import PlacementRequirementsSection from './PlacementRequirementsSection.svelte';

	type Requirements = components['schemas']['Requirements'];

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
		jobTemplateRef?: JobTemplateRef | null;
		jobTemplateParams?: Record<string, unknown>;
	};

	type ExtendedDeploymentModel =
		| { mode: 'executor'; capacity?: null | unknown }
		| ScheduledExtended;

	type Props = {
		value: DeploymentModelValue | undefined;
		/** Whether the Scheduled (Nomad/Slurm) target is offered. Engine-effect
		 * backends are inline-only; pass `false` to hide it. */
		schedulable?: boolean;
		/** The step's placement `requirements`. Placement constraints ONLY apply to
		 * the presence `capacity` model (capability-matched `t_grant`), so the editor is
		 * hosted here, in the Runner-group branch. Omit (Agent panel) to hide it. */
		requirements?: Requirements | null;
		onRequirementsChange?: (requirements: Requirements | undefined) => void;
		readonly?: boolean;
		onchange: (deploymentModel: DeploymentModelValue) => void;
	};

	let {
		value,
		schedulable = true,
		requirements,
		onRequirementsChange,
		readonly = false,
		onchange
	}: Props = $props();

	const ext = $derived(value as ExtendedDeploymentModel | undefined);
	const allowScheduled = $derived(schedulable);

	// ── Scheduled-mode derivations (unchanged) ────────────────────────────────
	const jobTemplate = $derived(value?.mode === 'scheduled' ? value.jobTemplate : '');
	const scheduler = $derived(value?.mode === 'scheduled' ? (value.scheduler ?? '') : '');
	const jobTemplateRef = $derived(ext?.mode === 'scheduled' ? (ext.jobTemplateRef ?? null) : null);
	const jobTemplateParams = $derived(ext?.mode === 'scheduled' ? (ext.jobTemplateParams ?? {}) : {});
	const pickerFlavor = $derived<string | null>(null);

	let showManualTemplate = $state(false);
	$effect(() => {
		if (ext?.mode === 'scheduled' && ext.jobTemplate && !ext.jobTemplateRef) {
			untrack(() => {
				showManualTemplate = true;
			});
		}
	});

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
		if (!ref) delete dm.jobTemplateParams;
		onchange(dm as unknown as DeploymentModelValue);
	}
	function setJobTemplateParams(params: Record<string, unknown>) {
		if (value?.mode !== 'scheduled') return;
		const dm = { ...value } as ScheduledExtended;
		dm.jobTemplateParams = Object.keys(params).length > 0 ? params : undefined;
		onchange(dm as unknown as DeploymentModelValue);
	}

	// ── Capacity binding ──────────────────────────────────────────────────────
	// The binding lives under `deploymentModel.Executor.capacity` and names a
	// single `capacity` resource. A capacity is discriminated by its `liveness`
	// axis — presence (a runner group) vs seeded (a concurrency limit) — which
	// this section presents as distinct "Run on" targets (rather than one generic
	// "capacity" claim). `alias` is REQUIRED for a bound step.
	const capacityAlias = $derived(
		value?.mode === 'executor'
			? ((value as { mode: 'executor'; capacity?: null | { alias: string } }).capacity?.alias ?? '')
			: ''
	);

	// ── "Run on" target — the deployment model, made first-class ──────────────
	// `target` is LOCAL UI state, not a pure derivation. An executor value with a
	// `capacity` object but an EMPTY alias is ambiguous (the runner-group vs limit
	// liveness can't be told apart from the value alone) — so when the user picks
	// "Runner group"/"Concurrency limit" (which writes `capacity:{alias:''}` until
	// they choose a resource), `resolveRunTarget` returns null and we KEEP the
	// local choice instead of snapping back to "Worker pool" (the reported bug).
	// The resolution logic is unit-tested in `deployment-run-target.test.ts`.
	// `untrack`: a deliberate one-time read of the initial `value` (the effect
	// owns ongoing sync) — silences the state_referenced_locally lint.
	let target = $state<Target>(untrack(() => initialRunTarget(value as DeploymentLike | undefined)));
	$effect(() => {
		const next = resolveRunTarget(value as DeploymentLike | undefined, targetByAlias);
		if (next !== null) {
			untrack(() => {
				if (target !== next) target = next;
			});
		}
	});

	// Options depend on the backend kind:
	//   ExecutorJob backend → worker pool / runner group / concurrency limit / scheduled
	//   engine-effect (!schedulable) → inline / concurrency limit (no worker or cluster placement)
	const targetOptions = $derived(
		allowScheduled
			? ([
					{ v: 'workers', label: 'Worker pool' },
					{ v: 'runner_group', label: 'Runner group' },
					{ v: 'limit', label: 'Concurrency limit' },
					{ v: 'scheduled', label: 'Scheduled cluster (Nomad / Slurm)' }
				] as { v: Target; label: string }[])
			: ([
					{ v: 'workers', label: 'Inline (engine effect)' },
					{ v: 'limit', label: 'Concurrency limit' }
				] as { v: Target; label: string }[])
	);

	function targetLabel(t: Target): string {
		const found = targetOptions.find((o) => o.v === t);
		if (found) return found.label;
		// A binding whose model isn't in the current option set (e.g. a legacy
		// runner_group on an engine-effect backend) still gets a sensible label.
		return t === 'runner_group'
			? 'Runner group'
			: t === 'scheduled'
				? 'Scheduled cluster (Nomad / Slurm)'
				: 'Worker pool';
	}

	function setTarget(t: Target) {
		if (t === target) return;
		// Set the local target immediately so a capacity choice with no alias yet
		// still shows its picker (the effect won't override an empty-alias value).
		target = t;
		if (t === 'scheduled') {
			onchange({ mode: 'scheduled', jobTemplate: '' } as unknown as DeploymentModelValue);
			return;
		}
		if (t === 'workers') {
			onchange({ mode: 'executor' } as DeploymentModelValue);
			return;
		}
		// runner_group | limit → executor + a capacity claim of the matching
		// liveness. Keep the current alias only if its capacity already maps to
		// this target; otherwise start empty (the picker + a "select a …" prompt
		// follow).
		const keep = capacityAlias && targetByAlias.get(capacityAlias) === t ? capacityAlias : '';
		onchange({ mode: 'executor', capacity: { alias: keep } } as DeploymentModelValue);
	}

	function setPoolAlias(alias: string) {
		if (value?.mode !== 'executor') return;
		// Preserve any existing request params when re-pointing the alias.
		const prev = value as {
			mode: 'executor';
			capacity?: null | { alias: string; request?: unknown };
		};
		const prevRequest = prev.capacity?.request;
		onchange({
			mode: 'executor',
			capacity: { alias, ...(prevRequest !== undefined ? { request: prevRequest } : {}) }
		} as DeploymentModelValue);
	}

	// ── Optional raw-JSON `request` params (kept as text locally so invalid JSON
	// mid-typing doesn't clobber the model; committed on valid parse). ──────────
	const requestValue = $derived(
		value?.mode === 'executor'
			? (value as { mode: 'executor'; capacity?: null | { alias: string; request?: unknown } })
					.capacity?.request
			: undefined
	);
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
			const extDm = dm as {
				mode: 'executor';
				capacity?: null | { alias: string; request?: unknown };
			};
			if (extDm.capacity == null) return;
			const capacity: { alias: string; request?: unknown } = { alias: extDm.capacity.alias };
			if (parsed !== undefined) capacity.request = parsed;
			onchange({ mode: 'executor', capacity } as DeploymentModelValue);
		}
	}

	// ── Resource pickers ──────────────────────────────────────────────────────
	// The single `capacity` kind loads eagerly (a small per-workspace list) so the
	// target derivation can resolve a bound alias → its liveness-derived target,
	// and each picker can filter to its own liveness. Each capacity's `liveness`
	// axis (in `public_config`, surfaced on the list summary) tells the presence
	// (runner group) and seeded (concurrency limit) buckets apart. Datacenter
	// resources load lazily when Scheduled is active.
	let capacities = $state<ResourceSummary[]>([]);
	let schedulerResources = $state<ResourceSummary[]>([]);
	let capacityLoaded = $state(false);
	let schedulerResourcesLoaded = $state(false);

	$effect(() => {
		if (capacityLoaded) return;
		capacityLoaded = true;
		listResources({ resource_type: 'capacity', perPage: 200 })
			.then((p) => {
				capacities = p.items;
			})
			.catch(() => {
				/* leave empty — pickers show the empty hint */
			});
	});
	$effect(() => {
		if (value?.mode === 'scheduled' && !schedulerResourcesLoaded) {
			schedulerResourcesLoaded = true;
			listResources({ resource_type: 'datacenter', perPage: 200 })
				.then((p) => (schedulerResources = p.items))
				.catch(() => {
					/* leave empty — picker shows the empty hint */
				});
		}
	});

	// alias → the run target its capacity maps to (by liveness). Drives both the
	// bound-alias resolution and each picker's per-liveness filter.
	const targetByAlias = $derived(targetsByAlias(capacities));
	const runnerGroups = $derived(capacities.filter((r) => capacityTarget(r) === 'runner_group'));
	const limits = $derived(capacities.filter((r) => capacityTarget(r) === 'limit'));

	/** The resource list for the active capacity target. */
	const capacityList = $derived(target === 'runner_group' ? runnerGroups : limits);

	function aliasLabel(): string {
		if (!capacityAlias) {
			return target === 'runner_group' ? 'Select a runner group…' : 'Select a concurrency limit…';
		}
		const found =
			capacityList.find((r) => r.path === capacityAlias) ??
			capacities.find((r) => r.path === capacityAlias);
		return found ? `${found.path} — ${found.display_name}` : capacityAlias;
	}
	function schedulerLabel(): string {
		if (!scheduler) return 'Select a datacenter resource…';
		const found = schedulerResources.find((r) => r.path === scheduler);
		return found ? `${found.path} — ${found.display_name}` : scheduler;
	}
</script>

<div class="space-y-2 pt-3 border-t border-border/40">
	<span class="text-sm font-medium text-muted-foreground">Deployment</span>

	<!-- One selector picks the deployment MODEL — where the step's work runs +
	     against what capacity. Each choice's controls + help sit directly below. -->
	<FormField label="Run on" for="deployment-target">
		<Select.Root
			type="single"
			value={target}
			onValueChange={(v) => {
				if (v) setTarget(v as Target);
			}}
			disabled={readonly}
		>
			<Select.Trigger
				id="deployment-target"
				disabled={readonly}
				data-testid="select-deployment-model"
			>
				<span class="truncate text-sm">{targetLabel(target)}</span>
			</Select.Trigger>
			<Select.Content>
				{#each targetOptions as o (o.v)}
					<Select.Item value={o.v} label={o.label} />
				{/each}
			</Select.Content>
		</Select.Root>
	</FormField>

	{#if target === 'runner_group' || target === 'limit' || target === 'scheduled'}
		<p class="text-xs italic text-muted-foreground">
			Sets the <strong>default binding</strong> for this template's home workspace. Forks and other
			workspaces bind their own resource under <em>Configure resources</em>, and each run can override
			it in the launch dialog.
		</p>
	{/if}

	{#if target === 'workers'}
		<p class="text-sm text-muted-foreground">
			{#if allowScheduled}
				Runs on any worker serving this step's backend — fungible capacity, routed by backend. No
				reservation held.
			{:else}
				Runs inline as an engine effect — no worker or cluster placement.
			{/if}
		</p>
	{:else if target === 'runner_group' || target === 'limit'}
		<FormField
			label={target === 'runner_group' ? 'Runner group' : 'Concurrency limit'}
			for="pool-alias"
		>
			<Select.Root
				type="single"
				value={capacityAlias}
				onValueChange={(v) => setPoolAlias(v ?? '')}
				disabled={readonly}
			>
				<Select.Trigger disabled={readonly} data-testid="select-pool-alias">
					<span class="truncate text-sm">{aliasLabel()}</span>
				</Select.Trigger>
				<Select.Content>
					{#each capacityList as r (r.id)}
						<Select.Item value={r.path} label={`${r.path} — ${r.display_name}`} />
					{/each}
				</Select.Content>
			</Select.Root>
		</FormField>
		{#if !capacityAlias}
			<p class="text-sm text-destructive">
				Select a {target === 'runner_group' ? 'runner group' : 'concurrency limit'} — this step must
				name a resource.
			</p>
		{:else if capacityList.length === 0 && capacityLoaded}
			<p class="text-sm italic text-muted-foreground">
				No <code class="font-mono">capacity</code> resources with
				{target === 'runner_group' ? 'presence' : 'seeded'} liveness in this workspace. Add one (the
				{target === 'runner_group' ? 'instrument' : 'limit'} preset) under
				<code class="font-mono">/resources</code> first.
			</p>
		{/if}
		{#if capacityAlias}
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
			{#if target === 'runner_group'}
				Pins the step to one enrolled runner in this group for its duration; queues when every runner
				is busy. The grant is readable in the body as <code>lease.unit_id</code>.
			{:else}
				Holds one unit of this concurrency limit for the step's duration; queues when the limit is
				reached. The grant is readable in the body as <code>lease.unit_id</code>.
			{/if}
		</p>
		<!-- Placement requirements belong to the runner_group model ONLY — the
		     engine's `satisfies()` guard runs only on the presence pool's t_grant.
		     Concurrency-limit/worker/scheduled steps carry no requirements. -->
		{#if target === 'runner_group' && onRequirementsChange}
			<PlacementRequirementsSection
				requirements={requirements ?? undefined}
				{readonly}
				onchange={onRequirementsChange}
			/>
		{/if}
	{:else if target === 'scheduled'}
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
					Overrides the picker above. Use when the job name is pre-registered on the cluster and does
					not yet have a control-plane template.
				</p>
			{/if}
		</div>

		<p class="text-sm text-muted-foreground">
			Leases a warm allocation from the datacenter for the step's duration; the granted lease is
			readable in the body as <code>lease.node</code> / <code>lease.alloc_id</code>.
		</p>
	{/if}
</div>
