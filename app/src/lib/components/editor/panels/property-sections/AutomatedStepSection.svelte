<script lang="ts">
	import type { AutomatedStepNodeData, ExecutionBackendType } from '$lib/types/editor';
	import type { components } from '$lib/api/schema';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import { onMount, untrack } from 'svelte';
	import { portsEqual } from '$lib/editor/port-utils';
	import { createDebouncedFetcher } from '$lib/editor/debounced-fetcher';
	import * as Select from '$lib/components/ui/select';
	import { Button } from '$lib/components/ui/button';
	import { Checkbox } from '$lib/components/ui/checkbox';
	import { Textarea } from '$lib/components/ui/textarea';
	import { listResources, type ResourceSummary } from '$lib/api/resources';
	import RotateCcw from '@lucide/svelte/icons/rotate-ccw';
	import PortsSection from './PortsSection.svelte';
	import { defaultOutputPort, emptyOutputPort } from '$lib/editor/automated-ports';
	import {
		backendList,
		deriveBackendOutput,
		getCachedBackend,
		loadBackends
	} from '$lib/editor/backend-registry.svelte';
	import { BACKEND_PANELS } from '$lib/editor/backend-panels';
	import { GPU_JOB_TEMPLATES } from '$lib/editor/deployment-targets';
	import { FormField } from '$lib/components/ui/form-field';
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';

	type Port = components['schemas']['Port'];

	type Props = {
		data: AutomatedStepNodeData;
		readonly?: boolean;
		onchange: (data: AutomatedStepNodeData) => void;
		binding?: YjsGraphBinding;
		nodeId?: string;
		templateId?: string;
		scope?: ScopeEntry[];
	};

	let {
		data,
		readonly = false,
		onchange,
		binding,
		nodeId,
		templateId,
		scope = []
	}: Props = $props();

	// Registry is loaded once in +layout.svelte; this is a safety net for
	// direct deep-links to editor routes that mount before the layout
	// finishes its onMount.
	onMount(() => {
		loadBackends().catch(() => {
			/* descriptors render empty; user sees a Loading… placeholder */
		});
	});

	const outputPort = $derived<Port>(data.output ?? emptyOutputPort());
	const backends = $derived(backendList());
	const currentBackend = $derived(getCachedBackend(data.executionSpec.backendType));
	const CurrentPanel = $derived(BACKEND_PANELS[data.executionSpec.backendType]);
	// `free` → user owns the port (current behavior, with Reset button).
	// `fixed` → backend's canonical default; read-only.
	// `derived` → server-derived from config; read-only and refetched on change.
	// Defaults to `free` so callers that pre-date the field (or hit the
	// registry before it loads) keep the legacy editable surface.
	const outputAuthoring = $derived(currentBackend?.outputAuthoring ?? 'free');

	function handleOutputPortChange(port: Port) {
		onchange({ ...data, output: port });
	}

	function resetOutputToBackendDefault() {
		onchange({ ...data, output: defaultOutputPort(data.executionSpec.backendType) });
	}

	// Derived-authoring effect: whenever the backend is `derived` and the
	// step config changes, fetch the canonical port shape from the server
	// and persist it onto `data.output`. The compiler is the source of
	// truth, so the editor never derives locally — drift is impossible.
	//
	// Debounced lightly (250ms) so rapid keystrokes in the JSON-schema
	// editor don't flood the endpoint. On failure we fall back to the
	// backend's default port shape from the descriptor rather than
	// silently keeping a stale `data.output`.
	const deriveFetcher = createDebouncedFetcher();
	$effect(() => {
		if (outputAuthoring !== 'derived' || readonly) return;
		const backendType = data.executionSpec.backendType;
		const cfg = data.executionSpec.config;
		deriveFetcher.schedule(async (fresh) => {
			try {
				const port = await deriveBackendOutput(backendType, cfg);
				if (!fresh()) return;
				untrack(() => {
					if (!portsEqual(data.output, port)) {
						onchange({ ...data, output: port });
					}
				});
			} catch {
				if (!fresh()) return;
				untrack(() => {
					const fallback = defaultOutputPort(backendType);
					if (!portsEqual(data.output, fallback)) {
						onchange({ ...data, output: fallback });
					}
				});
			}
		});
	});

	// Fixed-authoring effect: backend owns the canonical shape, so
	// overwrite any legacy/customized `data.output` with the descriptor's
	// default on first paint and whenever the backend changes. No debounce
	// — the descriptor is static.
	$effect(() => {
		if (outputAuthoring !== 'fixed' || readonly) return;
		const fixed = defaultOutputPort(data.executionSpec.backendType);
		untrack(() => {
			if (!portsEqual(data.output, fixed)) {
				onchange({ ...data, output: fixed });
			}
		});
	});

	function handleBackendTypeChange(backendType: ExecutionBackendType) {
		const decl = getCachedBackend(backendType);
		// Registry-supplied seed config. If the descriptor hasn't loaded
		// yet (rare — see onMount), fall back to an empty object; the user
		// can fill in fields manually until the panel renders.
		const defaultConfig =
			(decl?.defaultEditorConfig as Record<string, unknown> | undefined) ?? {};
		onchange({
			...data,
			executionSpec: {
				backendType,
				entrypoint:
					backendType === 'python'
						? (data.executionSpec.entrypoint ?? 'main.py')
						: data.executionSpec.entrypoint,
				config: defaultConfig
			}
		});
	}

	function handleConfigChange(config: Record<string, unknown>) {
		onchange({
			...data,
			executionSpec: { ...data.executionSpec, config }
		});
	}

	function handleEntrypointChange(entrypoint: string) {
		onchange({
			...data,
			executionSpec: { ...data.executionSpec, entrypoint }
		});
	}

	function isExecutionBackendType(v: string): v is ExecutionBackendType {
		return backends.some((b) => b.name === v);
	}

	// Deployment model — executor (our executor daemon pool over the NATS work
	// queue) vs scheduled (scheduler-net, Nomad/Slurm GPU). Optional-chained so
	// legacy templates (no field) render as executor; Rust `#[serde(default)]`
	// covers the wire side.
	const deploymentMode = $derived(data.deploymentModel?.mode ?? 'executor');
	const jobTemplate = $derived(
		data.deploymentModel?.mode === 'scheduled' ? data.deploymentModel.jobTemplate : ''
	);
	// Hide the Scheduled toggle for backends the registry marks
	// non-schedulable (engine effects — catalogue_query today).
	const allowScheduled = $derived(currentBackend?.schedulable ?? true);

	// Scheduled sub-fields. `operation` selects submit (today's dispatch) vs
	// lease (R4, requires a concrete datacenter scheduler alias). `scheduler` is
	// the datacenter resource alias; null/'' = env-global scheduler-net (only
	// valid for submit).
	const operation = $derived(
		data.deploymentModel?.mode === 'scheduled'
			? (data.deploymentModel.operation ?? 'submit')
			: 'submit'
	);
	const scheduler = $derived(
		data.deploymentModel?.mode === 'scheduled' ? (data.deploymentModel.scheduler ?? '') : ''
	);

	// Switching to scheduled drops any executor pool (pool is Executor-only — a
	// datacenter cluster is bound under Scheduled instead). Defaults to
	// operation=submit, env-global scheduler.
	function setDeploymentMode(mode: string) {
		onchange({
			...data,
			deploymentModel:
				mode === 'scheduled'
					? {
							mode: 'scheduled',
							jobTemplate: jobTemplate || GPU_JOB_TEMPLATES[0].value
						}
					: { mode: 'executor' }
		});
	}

	function setJobTemplate(v: string) {
		if (data.deploymentModel?.mode !== 'scheduled') return;
		onchange({ ...data, deploymentModel: { ...data.deploymentModel, jobTemplate: v } });
	}

	function setOperation(op: 'submit' | 'lease') {
		if (data.deploymentModel?.mode !== 'scheduled') return;
		onchange({ ...data, deploymentModel: { ...data.deploymentModel, operation: op } });
	}

	function setScheduler(alias: string) {
		if (data.deploymentModel?.mode !== 'scheduled') return;
		const dm = { ...data.deploymentModel };
		if (alias) dm.scheduler = alias;
		else delete dm.scheduler;
		onchange({ ...data, deploymentModel: dm });
	}

	// Executor-pool token admission. The binding lives under
	// `deploymentModel.Executor.pool` (post-R3 consolidation); presence = "claim
	// a unit from this token_pool". `alias` is REQUIRED — a pooled step names a
	// token_pool resource (no well-known-global fallback).
	const poolAlias = $derived(
		data.deploymentModel?.mode === 'executor' ? (data.deploymentModel.pool?.alias ?? '') : ''
	);
	const requiresPool = $derived(
		data.deploymentModel?.mode === 'executor' && data.deploymentModel.pool != null
	);
	// Pool is intrinsically executor-only now (it lives under Executor.pool), so
	// the control is simply hidden while scheduled.
	const poolControlsVisible = $derived(deploymentMode === 'executor');

	function setRequiresPool(on: boolean) {
		onchange({
			...data,
			deploymentModel: on
				? { mode: 'executor', pool: { alias: poolAlias } }
				: { mode: 'executor' }
		});
	}

	function setPoolAlias(alias: string) {
		if (data.deploymentModel?.mode !== 'executor') return;
		// Preserve any existing request params when re-pointing the alias.
		const prevRequest = data.deploymentModel.pool?.request;
		onchange({
			...data,
			deploymentModel: {
				mode: 'executor',
				pool: { alias, ...(prevRequest !== undefined ? { request: prevRequest } : {}) }
			}
		});
	}

	// ── Optional raw-JSON `request` params (v1: a textarea, not a schema form).
	// Bound to Executor.pool.request / Scheduled.request. Kept as text locally so
	// invalid JSON mid-typing doesn't clobber the model; committed on valid parse.
	const requestValue = $derived(
		data.deploymentModel?.mode === 'executor'
			? data.deploymentModel.pool?.request
			: data.deploymentModel?.mode === 'scheduled'
				? data.deploymentModel.request
				: undefined
	);
	let requestText = $state('');
	let requestError = $state<string | null>(null);
	// Re-seed the textarea from the model whenever the bound value changes
	// (node switch, alias re-point). `untrack` so editing requestText doesn't
	// re-trigger; we only follow the upstream value.
	$effect(() => {
		const v = requestValue;
		untrack(() => {
			requestText = v === undefined ? '' : JSON.stringify(v, null, 2);
			requestError = null;
		});
	});

	function commitRequest(text: string) {
		requestText = text;
		const dm = data.deploymentModel;
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
			onchange({ ...data, deploymentModel: { mode: 'executor', pool } });
		} else if (dm.mode === 'scheduled') {
			const next = { ...dm };
			if (parsed !== undefined) next.request = parsed;
			else delete next.request;
			onchange({ ...data, deploymentModel: next });
		}
	}

	// ── Resource pickers (load workspace resources filtered by kind) ──────────
	// Mirrors the shared ResourcePicker precedent (LLM/SMTP alias binding):
	// `listResources({ resource_type })` filters server-side by kind; we bind
	// the alias (`r.path`). Two independent lists — token_pool for the Executor
	// pool, datacenter for the Scheduled scheduler.
	let poolResources = $state<ResourceSummary[]>([]);
	let schedulerResources = $state<ResourceSummary[]>([]);
	let poolResourcesLoaded = $state(false);
	let schedulerResourcesLoaded = $state(false);

	// Load lazily, once each, when the relevant branch first becomes visible.
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
		if (!scheduler) return 'Environment default (no datacenter resource)';
		const found = schedulerResources.find((r) => r.path === scheduler);
		return found ? `${found.path} — ${found.display_name}` : scheduler;
	}

	// Streaming side-channel. When on, the compiler synthesizes a Signal place
	// `p_{id}_stream` and routes the runner's log events (SDK `log_info()` …)
	// into it — one token per log event — exposed as a second "stream" output
	// port. The normal control "out" token still governs termination; "stream"
	// is purely additive. Optional-chained for legacy data (absent → false);
	// Rust `#[serde(default)]` covers the wire side.
	const streamOutput = $derived(data.streamOutput ?? false);

	function setStreamOutput(on: boolean) {
		onchange({ ...data, streamOutput: on });
	}
</script>

<div class="space-y-1.5">
	<span class="text-sm font-medium text-muted-foreground">Backend Type</span>
	<Select.Root
		type="single"
		value={data.executionSpec.backendType}
		onValueChange={(v) => {
			if (v && isExecutionBackendType(v)) handleBackendTypeChange(v);
		}}
		disabled={readonly}
	>
		<Select.Trigger disabled={readonly}>
			{currentBackend?.displayName ?? data.executionSpec.backendType}
		</Select.Trigger>
		<Select.Content>
			{#each backends as b (b.name)}
				<Select.Item value={b.name} label={b.displayName} />
			{/each}
		</Select.Content>
	</Select.Root>
</div>

{#if CurrentPanel}
	<CurrentPanel
		config={data.executionSpec.config as Record<string, unknown>}
		entrypoint={data.executionSpec.entrypoint ?? 'main.py'}
		{readonly}
		onchange={handleConfigChange}
		onentrypointchange={handleEntrypointChange}
		{binding}
		{nodeId}
		{templateId}
		{scope}
	/>
{/if}

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
					? 'Scheduled (Nomad/Slurm, GPU)'
					: 'Executor (worker pool)'}
			</Select.Trigger>
			<Select.Content>
				<Select.Item value="executor" label="Executor (worker pool)" />
				<Select.Item value="scheduled" label="Scheduled (Nomad/Slurm, GPU)" />
			</Select.Content>
		</Select.Root>
		{#if deploymentMode === 'scheduled'}
			<!-- Operation: submit (today's dispatch) vs lease (R4 datacenter lease).
			     Lease REQUIRES a concrete datacenter scheduler alias. -->
			<FormField label="Operation" for="scheduled-operation">
				<Select.Root
					type="single"
					value={operation}
					onValueChange={(v) => {
						if (v === 'submit' || v === 'lease') setOperation(v);
					}}
					disabled={readonly}
				>
					<Select.Trigger disabled={readonly} data-testid="select-scheduled-operation">
						{operation === 'lease' ? 'Lease (hold an allocation)' : 'Submit (dispatch a job)'}
					</Select.Trigger>
					<Select.Content>
						<Select.Item value="submit" label="Submit (dispatch a job)" />
						<Select.Item value="lease" label="Lease (hold an allocation)" />
					</Select.Content>
				</Select.Root>
			</FormField>

			<!-- Datacenter scheduler resource. Optional for submit (env-global
			     scheduler-net when unset); required for lease. -->
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
						<Select.Item value="" label="Environment default (no datacenter resource)" />
						{#each schedulerResources as r (r.id)}
							<Select.Item value={r.path} label={`${r.path} — ${r.display_name}`} />
						{/each}
					</Select.Content>
				</Select.Root>
			</FormField>
			{#if operation === 'lease' && !scheduler}
				<p class="text-sm text-destructive">
					Lease requires a datacenter resource — select one above (the
					environment-default scheduler only supports Submit).
				</p>
			{:else if schedulerResources.length === 0 && schedulerResourcesLoaded}
				<p class="text-sm italic text-muted-foreground">
					No <code class="font-mono">datacenter</code> resources in this workspace.
					Add one under <code class="font-mono">/resources</code> to lease external
					cluster allocations.
				</p>
			{/if}

			{#if operation === 'submit'}
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
			{/if}

			<FormField label="Request (optional)" for="scheduled-request">
				<Textarea
					id="scheduled-request"
					class="font-mono text-sm"
					rows={3}
					value={requestText}
					disabled={readonly}
					placeholder={'{ "gpu_count": 1, "gpu_type": "a100" }'}
					oninput={(e) => commitRequest((e.currentTarget as HTMLTextAreaElement).value)}
					data-testid="textarea-scheduled-request"
				/>
			</FormField>
			{#if requestError}
				<p class="text-sm text-destructive">{requestError}</p>
			{/if}
			<p class="text-sm text-muted-foreground">
				{operation === 'lease'
					? 'Lease params validated against the datacenter kind’s claim schema; the granted lease is readable in the body as lease.node / lease.gpu_uuid / lease.alloc_id.'
					: 'Submitted through the scheduler-net, which owns queueing, GPU allocation and retry/backoff.'}
			</p>
		{/if}
	{:else}
		<p class="text-sm text-muted-foreground">
			Executor only — this backend runs as an engine effect.
		</p>
	{/if}

	<!-- Executor-pool token admission. Lives under Executor.pool — only shown
	     for executor steps. A pooled step names a token_pool resource by alias
	     (required); the alias picker filters workspace resources by kind. -->
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
						No <code class="font-mono">token_pool</code> resources in this workspace.
						Add one under <code class="font-mono">/resources</code> first.
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
					Holds a unit from the named token_pool resource for the step's
					duration; queues when the pool is full. The granted lease is
					readable in the body as <code>lease.unit_id</code>.
				</p>
			{/if}
		</div>
	{/if}
</div>

<div class="space-y-1 pt-3 border-t border-border/40">
	<span class="text-sm font-medium text-muted-foreground">Streaming</span>
	<label class="flex items-center gap-1.5 text-sm text-foreground">
		<Checkbox
			checked={streamOutput}
			disabled={readonly}
			onCheckedChange={(v) => setStreamOutput(v === true)}
			data-testid="toggle-stream-output"
		/>
		Stream output
	</label>
	<p class="text-sm text-muted-foreground">
		Adds a second <code class="font-mono">stream</code> output port that emits one
		token per log event (the runner's <code class="font-mono">log_info()</code> …
		calls) while the step runs. A downstream node wired from it fires once per
		token. The normal <code class="font-mono">out</code> port still governs
		completion — this is an additive side-channel.
	</p>
</div>

<div class="space-y-2 pt-3 border-t border-border/40">
	<div class="flex items-center justify-between">
		<span class="text-sm font-medium text-muted-foreground">Output port</span>
		{#if !readonly && outputAuthoring === 'free'}
			<Button
				variant="ghost"
				size="sm"
				onclick={resetOutputToBackendDefault}
				class="h-7 gap-1 px-2 text-sm"
				title="Reset output port to the backend's canonical shape"
			>
				<RotateCcw class="size-3.5" />
				Reset to {data.executionSpec.backendType} default
			</Button>
		{/if}
	</div>
	{#if outputAuthoring === 'derived'}
		<p class="text-sm text-muted-foreground">
			Derived from this step's config — edit the config above (response format, schema) to change the
			output fields. The runner produces this exact shape; declaring it manually would only drift.
		</p>
	{:else if outputAuthoring === 'fixed'}
		<p class="text-sm text-muted-foreground">
			Fixed canonical shape for this backend. The runner always emits these fields.
		</p>
	{/if}
	<PortsSection
		port={outputPort}
		readonly={readonly || outputAuthoring !== 'free'}
		title="Fields"
		emptyHint={outputAuthoring === 'derived'
			? 'No fields yet — pick a response format on this step to define them.'
			: "No declared output fields. Downstream edges with declared input ports will type-mismatch on publish — click reset to seed the backend's default shape."}
		onchange={handleOutputPortChange}
	/>
</div>
