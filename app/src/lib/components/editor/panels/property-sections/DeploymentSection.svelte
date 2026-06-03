<script lang="ts">
	import type { AutomatedStepNodeData } from '$lib/types/editor';
	import { untrack } from 'svelte';
	import * as Select from '$lib/components/ui/select';
	import { Checkbox } from '$lib/components/ui/checkbox';
	import { Textarea } from '$lib/components/ui/textarea';
	import { Input } from '$lib/components/ui/input';
	import { FormField } from '$lib/components/ui/form-field';
	import { listResources, type ResourceSummary } from '$lib/api/resources';
	import JobTemplatePicker, { type JobTemplateRef } from './shared/JobTemplatePicker.svelte';
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
		readonly?: boolean;
		onchange: (deploymentModel: DeploymentModelValue) => void;
	};

	let { value, schedulable = true, readonly = false, onchange }: Props = $props();

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
	// `runner_group` OR a `concurrency_limit` resource — two semantically distinct
	// models that this section presents as distinct "Run on" targets (rather than
	// one generic "capacity" claim). `alias` is REQUIRED for a bound step.
	const capacityAlias = $derived(
		value?.mode === 'executor'
			? ((value as { mode: 'executor'; capacity?: null | { alias: string } }).capacity?.alias ?? '')
			: ''
	);

	// ── "Run on" target — the deployment model, made first-class ──────────────
	type Target = 'workers' | 'runner_group' | 'limit' | 'scheduled';

	/** The target is DERIVED from the persisted value (+ the resolved kind of any
	    bound alias), so the selector is a controlled reflection of the model. */
	const target = $derived.by((): Target => {
		if (value?.mode === 'scheduled') return 'scheduled';
		if (!capacityAlias) return 'workers';
		return kindByAlias.get(capacityAlias) === 'runner_group' ? 'runner_group' : 'limit';
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
		if (t === 'scheduled') {
			onchange({ mode: 'scheduled', jobTemplate: '' } as unknown as DeploymentModelValue);
			return;
		}
		if (t === 'workers') {
			onchange({ mode: 'executor' } as DeploymentModelValue);
			return;
		}
		// runner_group | limit → executor + a capacity claim of the matching kind.
		// Keep the current alias only if it already matches the target's kind;
		// otherwise start empty (the picker + a "select a …" prompt follow).
		const wantKind = t === 'runner_group' ? 'runner_group' : 'concurrency_limit';
		const keep = capacityAlias && kindByAlias.get(capacityAlias) === wantKind ? capacityAlias : '';
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
	// Both capacity kinds load eagerly (small per-workspace lists) so the target
	// derivation can resolve a bound alias → its kind, and each picker can filter
	// to its own kind. Datacenter resources load lazily when Scheduled is active.
	let runnerGroups = $state<ResourceSummary[]>([]);
	let limits = $state<ResourceSummary[]>([]);
	let schedulerResources = $state<ResourceSummary[]>([]);
	let capacityLoaded = $state(false);
	let schedulerResourcesLoaded = $state(false);

	$effect(() => {
		if (capacityLoaded) return;
		capacityLoaded = true;
		Promise.all([
			listResources({ resource_type: 'runner_group', perPage: 200 }),
			listResources({ resource_type: 'concurrency_limit', perPage: 200 })
		])
			.then(([rg, cl]) => {
				runnerGroups = rg.items;
				limits = cl.items;
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

	const kindByAlias = $derived.by(() => {
		const m = new Map<string, 'runner_group' | 'concurrency_limit'>();
		for (const r of runnerGroups) m.set(r.path, 'runner_group');
		for (const r of limits) m.set(r.path, 'concurrency_limit');
		return m;
	});

	/** The resource list for the active capacity target. */
	const capacityList = $derived(target === 'runner_group' ? runnerGroups : limits);

	function aliasLabel(): string {
		if (!capacityAlias) {
			return target === 'runner_group' ? 'Select a runner group…' : 'Select a concurrency limit…';
		}
		const found =
			capacityList.find((r) => r.path === capacityAlias) ??
			[...runnerGroups, ...limits].find((r) => r.path === capacityAlias);
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
				No <code class="font-mono">{target === 'runner_group' ? 'runner_group' : 'concurrency_limit'}</code>
				resources in this workspace. Add one under <code class="font-mono">/resources</code> first.
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
