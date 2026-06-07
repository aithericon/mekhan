<script lang="ts">
	// SET tab — the operator-curated model set (GET /api/v1/models): every model
	// approved into the pool, each decorated with its lifecycle `state`, the
	// `available` AND-gate (state == loaded AND a live runner advertises it — the
	// flag the editor model picker filters on), and `serving_runners` (the count
	// of LIVE runners whose interface catalog advertises the model — the actual
	// serving signal, distinct from the manual `replicas` number).
	//
	// Actions are UNIFIED (no free-form state-machine select): per row the operator
	// can Load it onto a specific runner (RunnerTargetPicker → POST .../load),
	// Unload it from a runner (POST .../unload), or Delete the curated row
	// (DELETE /api/v1/models/{id}). Errors surface as toasts.
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import * as Dialog from '$lib/components/ui/dialog';
	import * as Select from '$lib/components/ui/select';
	import Boxes from '@lucide/svelte/icons/boxes';
	import Plus from '@lucide/svelte/icons/plus';
	import Minus from '@lucide/svelte/icons/minus';
	import { toast } from 'svelte-sonner';
	import {
		listLoadedModels,
		loadModel,
		unloadModel,
		createModel,
		deleteModel,
		setModelPolicy,
		clearModelPolicy,
		scaleModel,
		listNodePools,
		apiErrorMessage,
		type ModelSetView,
		type AutoscalePolicyInput
	} from '$lib/api/models';
	import type { ResourceSummary } from '$lib/api/resources';
	import RunnerTargetPicker from '$lib/components/fleet/RunnerTargetPicker.svelte';
	import { statusTone } from '$lib/components/fleet/model-pool';

	let models = $state<ModelSetView[]>([]);
	let busy = $state<string | null>(null);
	let nodePools = $state<ResourceSummary[]>([]);

	type AutoscaleMode = 'manual' | 'scale_to_zero' | 'keep_warm';

	// Autoscale-policy editor dialog state (one shared dialog, prefilled on open).
	let policyFor = $state<string | null>(null);
	let policyMode = $state<AutoscaleMode>('manual');
	let policyDesired = $state<string>('');
	let policyNodePool = $state<string>('');
	let policyZone = $state<string>('');
	let policyCooldown = $state<string>('');
	let policyDedicated = $state(false);
	let policyScaleUp = $state<string>('');
	let policyScaleDown = $state<string>('');
	let policySaving = $state(false);

	// Add-model dialog state.
	let addOpen = $state(false);
	let newModelId = $state('');
	let newBase = $state('');
	let adding = $state(false);

	// Load dialog state — pick a target runner for the model being loaded.
	let loadFor = $state<string | null>(null);
	let loadRunner = $state<string | null>(null);

	// Unload confirmation — pick the runner to evict the model from.
	let unloadFor = $state<string | null>(null);
	let unloadRunner = $state<string | null>(null);

	// Delete-from-pool confirmation.
	let deleteFor = $state<string | null>(null);

	async function poll() {
		try {
			models = await listLoadedModels();
		} catch (err) {
			toast.error(apiErrorMessage(err));
		}
	}

	async function loadPools() {
		try {
			nodePools = await listNodePools();
		} catch {
			// non-fatal — the editor falls back to its empty-state hint.
		}
	}

	$effect(() => {
		void poll();
		void loadPools();
		const t = setInterval(() => void poll(), 5000);
		return () => clearInterval(t);
	});

	function openPolicy(m: ModelSetView) {
		void loadPools();
		policyFor = m.model_id;
		const a = m.autoscale;
		policyMode = (a?.mode as AutoscaleMode) ?? 'manual';
		policyDesired = a?.desired_replicas != null ? String(a.desired_replicas) : '';
		policyNodePool = a?.node_pool ?? '';
		policyZone = a?.residency_zone ?? '';
		policyCooldown = a?.cooldown_secs != null ? String(a.cooldown_secs) : '';
		policyDedicated = a?.dedicated ?? false;
		policyScaleUp = a?.scale_up_threshold != null ? String(a.scale_up_threshold) : '';
		policyScaleDown = a?.scale_down_threshold != null ? String(a.scale_down_threshold) : '';
	}

	function numOrNull(s: string): number | null {
		const t = s.trim();
		if (!t) return null;
		const n = Number(t);
		return Number.isFinite(n) ? n : null;
	}

	async function savePolicy() {
		if (!policyFor || !policyNodePool.trim()) return;
		const modelId = policyFor;
		const body: AutoscalePolicyInput = {
			mode: policyMode,
			node_pool: policyNodePool.trim(),
			desired_replicas: numOrNull(policyDesired),
			residency_zone: policyZone.trim() || null,
			cooldown_secs: numOrNull(policyCooldown),
			dedicated: policyDedicated,
			scale_up_threshold: policyMode === 'manual' ? null : numOrNull(policyScaleUp),
			scale_down_threshold: policyMode === 'manual' ? null : numOrNull(policyScaleDown)
		};
		policySaving = true;
		try {
			await setModelPolicy(modelId, body);
			policyFor = null;
			await poll();
		} catch (err) {
			toast.error(apiErrorMessage(err));
		} finally {
			policySaving = false;
		}
	}

	async function disablePolicy(modelId: string) {
		busy = modelId;
		try {
			await clearModelPolicy(modelId);
			await poll();
		} catch (err) {
			toast.error(apiErrorMessage(err));
		} finally {
			busy = null;
		}
	}

	async function doScale(modelId: string, desired: number) {
		if (desired < 0) return;
		busy = modelId;
		try {
			await scaleModel(modelId, desired);
			await poll();
		} catch (err) {
			toast.error(apiErrorMessage(err));
		} finally {
			busy = null;
		}
	}

	async function doAdd() {
		const id = newModelId.trim();
		if (!id) return;
		adding = true;
		try {
			await createModel({ model_id: id, base: newBase.trim() || null });
			toast.success(`Added ${id} to the pool`);
			newModelId = '';
			newBase = '';
			addOpen = false;
			await poll();
		} catch (err) {
			toast.error(apiErrorMessage(err));
		} finally {
			adding = false;
		}
	}

	function openLoad(modelId: string) {
		loadFor = modelId;
		loadRunner = null;
	}

	async function doLoad() {
		if (!loadFor || !loadRunner) return;
		const modelId = loadFor;
		busy = modelId;
		try {
			await loadModel(modelId, loadRunner);
			loadFor = null;
			await poll();
		} catch (err) {
			toast.error(apiErrorMessage(err));
		} finally {
			busy = null;
		}
	}

	async function doUnload() {
		if (!unloadFor || !unloadRunner) return;
		const modelId = unloadFor;
		busy = modelId;
		try {
			await unloadModel(modelId, unloadRunner);
			unloadFor = null;
			await poll();
		} catch (err) {
			toast.error(apiErrorMessage(err));
		} finally {
			busy = null;
		}
	}

	async function doDelete() {
		if (!deleteFor) return;
		const modelId = deleteFor;
		busy = modelId;
		try {
			await deleteModel(modelId);
			toast.success(`Removed ${modelId} from the pool`);
			deleteFor = null;
			await poll();
		} catch (err) {
			toast.error(apiErrorMessage(err));
		} finally {
			busy = null;
		}
	}
</script>

<div class="space-y-4" data-testid="models-set">
	<div class="flex items-baseline gap-3">
		<h2 class="text-base font-semibold tracking-tight text-foreground">Curated model set</h2>
		<span class="text-sm text-muted-foreground">approved into the pool · live-runner AND-gate</span>
		<Button
			variant="outline"
			size="sm"
			class="ml-auto h-7 shrink-0 gap-1 px-2 text-sm"
			data-testid="add-model"
			onclick={() => (addOpen = true)}
		>
			<Plus class="size-3.5" />
			Add model
		</Button>
	</div>

	{#if models.length === 0}
		<div
			class="flex flex-col items-center gap-2 rounded-lg border border-dashed border-border/60 py-10 text-sm text-muted-foreground"
		>
			<Boxes class="size-8 text-muted-foreground/40" />
			No curated models. Use <b>Add model</b> to approve a model into the pool.
		</div>
	{:else}
		<div class="grid gap-2 sm:grid-cols-2">
			{#each models as m (m.model_id)}
				<div class="rounded-lg border border-border/60 bg-card p-3 text-sm" data-testid="model-set-row">
					<div class="flex items-center gap-2">
						<span
							class="size-1.5 shrink-0 rounded-full {m.available
								? 'bg-emerald-500'
								: 'bg-muted-foreground/40'}"
							title={m.available ? 'available (loaded + a live runner serves it)' : 'not available'}
						></span>
						<span class="truncate font-medium text-foreground">{m.model_id}</span>
						<span class="ml-auto text-sm {statusTone(String(m.state))}">{m.state}</span>
					</div>
					<div class="mt-0.5 pl-3.5 text-sm text-muted-foreground">
						{#if m.base}LoRA of {m.base} · {/if}served by {m.serving_runners} runner{m.serving_runners ===
						1
							? ''
							: 's'} · <span class="text-muted-foreground/70">{m.replicas} replica{m.replicas === 1
							? ''
							: 's'}</span>
						{#if m.note}· {m.note}{/if}
					</div>

					{#if m.state === 'loaded' && m.serving_runners === 0}
						<div
							class="mt-2 flex items-center gap-2 rounded-md border border-amber-200 bg-amber-50 px-2 py-1 text-sm text-amber-800 dark:border-amber-800/50 dark:bg-amber-950/40 dark:text-amber-200"
							data-testid="loaded-no-runner-hint"
						>
							<span class="min-w-0 flex-1"
								>loaded but no live runner serves it — load it on a runner</span
							>
							<Button
								variant="outline"
								size="sm"
								class="h-6 shrink-0 px-2 text-sm"
								disabled={busy !== null}
								onclick={() => openLoad(m.model_id)}
							>
								Load
							</Button>
						</div>
					{/if}

					{#if m.autoscale}
						{@const a = m.autoscale}
						<div
							class="mt-2 rounded-md border border-border/50 bg-muted/30 px-2 py-1.5 text-sm"
							data-testid="autoscale-block"
						>
							<div class="flex flex-wrap items-center gap-x-2 gap-y-1">
								<span class="text-muted-foreground">
									desired <b class="text-foreground"
										>{a.desired_count ?? a.desired_replicas ?? '—'}</b
									>
									· loaded <b class="text-foreground">{m.serving_runners}</b> ·
									<span class="text-foreground/80">{a.mode}</span>
								</span>
								{#if a.mode === 'manual'}
									{@const cur = a.desired_count ?? a.desired_replicas ?? 0}
									<div class="flex items-center gap-1">
										<Button
											variant="outline"
											size="sm"
											class="size-6 p-0"
											disabled={busy !== null || cur <= 0}
											data-testid="autoscale-dec"
											onclick={() => doScale(m.model_id, cur - 1)}
										>
											<Minus class="size-3" />
										</Button>
										<Button
											variant="outline"
											size="sm"
											class="size-6 p-0"
											disabled={busy !== null}
											data-testid="autoscale-inc"
											onclick={() => doScale(m.model_id, cur + 1)}
										>
											<Plus class="size-3" />
										</Button>
									</div>
								{/if}
								{#if a.status}<span class="{statusTone(a.status)} text-sm">{a.status}</span>{/if}
							</div>
							<div class="mt-1 flex flex-wrap items-center gap-1.5 text-sm text-muted-foreground">
								{#if a.node_pool}<span
										class="rounded bg-muted px-1.5 py-0.5 text-muted-foreground/80"
										>pool {a.node_pool}</span
									>{/if}
								{#if a.residency_zone}<span
										class="rounded bg-muted px-1.5 py-0.5 text-muted-foreground/80"
										>zone {a.residency_zone}</span
									>{/if}
								{#if a.dedicated}<span
										class="rounded bg-muted px-1.5 py-0.5 text-muted-foreground/80">dedicated</span
									>{/if}
							</div>
							{#if a.last_error}
								<div class="mt-1 text-sm text-red-600 dark:text-red-400">{a.last_error}</div>
							{/if}
							<div class="mt-1.5 flex items-center gap-1.5">
								<Button
									variant="outline"
									size="sm"
									class="h-6 px-2 text-sm"
									disabled={busy !== null}
									data-testid="autoscale-edit"
									onclick={() => openPolicy(m)}
								>
									Autoscale
								</Button>
								<Button
									variant="ghost"
									size="sm"
									class="h-6 px-2 text-sm text-muted-foreground"
									disabled={busy !== null}
									data-testid="autoscale-disable"
									onclick={() => disablePolicy(m.model_id)}
								>
									Disable
								</Button>
							</div>
						</div>
					{:else}
						<div class="mt-2">
							<Button
								variant="ghost"
								size="sm"
								class="h-6 px-2 text-sm text-muted-foreground"
								disabled={busy !== null}
								data-testid="autoscale-enable"
								onclick={() => openPolicy(m)}
							>
								Enable autoscaling
							</Button>
						</div>
					{/if}

					<div class="mt-2 flex items-center gap-1.5 border-t border-border/40 pt-2">
						<Button
							variant="outline"
							size="sm"
							class="h-7 px-2 text-sm"
							disabled={busy !== null}
							onclick={() => openLoad(m.model_id)}
						>
							{busy === m.model_id && loadFor === null ? '…' : 'Load'}
						</Button>
						<Button
							variant="outline"
							size="sm"
							class="h-7 px-2 text-sm"
							disabled={busy !== null}
							onclick={() => {
								unloadFor = m.model_id;
								unloadRunner = null;
							}}
						>
							Unload
						</Button>
						<Button
							variant="outline"
							size="sm"
							class="ml-auto h-7 px-2 text-sm text-red-600 hover:text-red-600 dark:text-red-400"
							disabled={busy !== null}
							onclick={() => (deleteFor = m.model_id)}
						>
							Delete
						</Button>
					</div>
				</div>
			{/each}
		</div>
	{/if}
</div>

<!-- Add model -->
<Dialog.Root bind:open={addOpen}>
	<Dialog.Content class="sm:max-w-md" data-testid="add-model-dialog">
		<Dialog.Header>
			<Dialog.Title>Add model to the pool</Dialog.Title>
			<Dialog.Description>
				Curate a model into the workspace SET. It lands in <code>approved</code> with zero replicas;
				load it onto a runner to serve it.
			</Dialog.Description>
		</Dialog.Header>
		<div class="space-y-3 py-1">
			<label class="block space-y-1">
				<span class="text-sm text-muted-foreground">Model id</span>
				<Input bind:value={newModelId} placeholder="llama3.1:8b" class="text-sm" />
			</label>
			<label class="block space-y-1">
				<span class="text-sm text-muted-foreground">Base model <span class="text-muted-foreground/60">(optional — for a LoRA)</span></span>
				<Input bind:value={newBase} placeholder="llama3.1:8b" class="text-sm" />
			</label>
		</div>
		<Dialog.Footer>
			<Button variant="ghost" size="sm" class="text-sm" onclick={() => (addOpen = false)}>Cancel</Button>
			<Button
				size="sm"
				class="text-sm"
				disabled={adding || !newModelId.trim()}
				data-testid="add-model-confirm"
				onclick={doAdd}
			>
				{adding ? 'Adding…' : 'Add'}
			</Button>
		</Dialog.Footer>
	</Dialog.Content>
</Dialog.Root>

<!-- Load onto a runner -->
<Dialog.Root open={loadFor !== null} onOpenChange={(o) => !o && (loadFor = null)}>
	<Dialog.Content class="sm:max-w-md" data-testid="load-model-dialog">
		<Dialog.Header>
			<Dialog.Title>Load model</Dialog.Title>
			<Dialog.Description>
				Pick a live runner to load <code class="font-mono">{loadFor}</code> onto.
			</Dialog.Description>
		</Dialog.Header>
		<div class="py-1">
			<RunnerTargetPicker value={loadRunner} onChange={(id) => (loadRunner = id)} />
		</div>
		<Dialog.Footer>
			<Button variant="ghost" size="sm" class="text-sm" onclick={() => (loadFor = null)}>Cancel</Button>
			<Button
				size="sm"
				class="text-sm"
				disabled={busy !== null || !loadRunner}
				data-testid="load-model-confirm"
				onclick={doLoad}
			>
				{busy !== null ? 'Loading…' : 'Load'}
			</Button>
		</Dialog.Footer>
	</Dialog.Content>
</Dialog.Root>

<!-- Unload from a runner -->
<Dialog.Root open={unloadFor !== null} onOpenChange={(o) => !o && (unloadFor = null)}>
	<Dialog.Content class="sm:max-w-md" data-testid="unload-model-dialog">
		<Dialog.Header>
			<Dialog.Title>Unload model</Dialog.Title>
			<Dialog.Description>
				Pick the runner to evict <code class="font-mono">{unloadFor}</code> from. The row moves to
				<code>draining</code> and an unload command is published to that runner.
			</Dialog.Description>
		</Dialog.Header>
		<div class="py-1">
			<RunnerTargetPicker value={unloadRunner} onChange={(id) => (unloadRunner = id)} />
		</div>
		<Dialog.Footer>
			<Button variant="ghost" size="sm" class="text-sm" onclick={() => (unloadFor = null)}>Cancel</Button>
			<Button
				variant="destructive"
				size="sm"
				class="text-sm"
				disabled={busy !== null || !unloadRunner}
				data-testid="unload-model-confirm"
				onclick={doUnload}
			>
				{busy !== null ? 'Unloading…' : 'Unload'}
			</Button>
		</Dialog.Footer>
	</Dialog.Content>
</Dialog.Root>

<!-- Delete from pool -->
<Dialog.Root open={deleteFor !== null} onOpenChange={(o) => !o && (deleteFor = null)}>
	<Dialog.Content class="sm:max-w-md" data-testid="delete-model-dialog">
		<Dialog.Header>
			<Dialog.Title>Delete from pool</Dialog.Title>
			<Dialog.Description>
				Remove <code class="font-mono">{deleteFor}</code> from the curated set. This deletes the
				lifecycle row only — it does not evict any live runner.
			</Dialog.Description>
		</Dialog.Header>
		<Dialog.Footer>
			<Button variant="ghost" size="sm" class="text-sm" onclick={() => (deleteFor = null)}>Cancel</Button>
			<Button
				variant="destructive"
				size="sm"
				class="text-sm"
				disabled={busy !== null}
				data-testid="delete-model-confirm"
				onclick={doDelete}
			>
				{busy !== null ? 'Deleting…' : 'Delete'}
			</Button>
		</Dialog.Footer>
	</Dialog.Content>
</Dialog.Root>

<!-- Autoscale policy editor -->
<Dialog.Root open={policyFor !== null} onOpenChange={(o) => !o && (policyFor = null)}>
	<Dialog.Content class="sm:max-w-md" data-testid="autoscale-dialog">
		<Dialog.Header>
			<Dialog.Title>Autoscaling</Dialog.Title>
			<Dialog.Description>
				Fold an autoscale policy onto <code class="font-mono">{policyFor}</code>. The autoscaler
				manages the replica COUNT on the chosen pool.
			</Dialog.Description>
		</Dialog.Header>
		<div class="space-y-3 py-1">
			<label class="block space-y-1">
				<span class="text-sm text-muted-foreground">Mode</span>
				<Select.Root
					type="single"
					value={policyMode}
					onValueChange={(v) => (policyMode = (v ?? 'manual') as AutoscaleMode)}
				>
					<Select.Trigger class="w-full text-sm" data-testid="autoscale-mode">
						{policyMode}
					</Select.Trigger>
					<Select.Content>
						<Select.Item value="manual" label="manual" />
						<Select.Item value="scale_to_zero" label="scale_to_zero" />
						<Select.Item value="keep_warm" label="keep_warm" />
					</Select.Content>
				</Select.Root>
			</label>

			<label class="block space-y-1">
				<span class="text-sm text-muted-foreground"
					>Desired / ceiling <span class="text-muted-foreground/60">(replicas)</span></span
				>
				<Input
					type="number"
					min="0"
					bind:value={policyDesired}
					placeholder="1"
					class="text-sm"
					data-testid="autoscale-desired"
				/>
			</label>

			<label class="block space-y-1">
				<span class="text-sm text-muted-foreground">Node pool <span class="text-red-500">*</span></span>
				{#if nodePools.length === 0}
					<p class="text-sm text-muted-foreground/70">
						No node pools yet. Create one on the <a
							href="/models/placement"
							class="font-medium text-foreground underline underline-offset-2 hover:text-primary"
							>Pools tab</a
						> first.
					</p>
				{:else}
					<Select.Root
						type="single"
						value={policyNodePool}
						onValueChange={(v) => (policyNodePool = v ?? '')}
					>
						<Select.Trigger class="w-full text-sm" data-testid="autoscale-pool">
							{policyNodePool || '— select a pool —'}
						</Select.Trigger>
						<Select.Content>
							{#each nodePools as p (p.id)}
								<Select.Item value={p.path} label={p.display_name || p.path} />
							{/each}
						</Select.Content>
					</Select.Root>
				{/if}
			</label>

			{#if policyMode !== 'manual'}
				<div class="grid grid-cols-2 gap-2">
					<label class="block space-y-1">
						<span class="text-sm text-muted-foreground">Scale-up threshold</span>
						<Input
							type="number"
							step="any"
							bind:value={policyScaleUp}
							placeholder="0.8"
							class="text-sm"
							data-testid="autoscale-up"
						/>
					</label>
					<label class="block space-y-1">
						<span class="text-sm text-muted-foreground">Scale-down threshold</span>
						<Input
							type="number"
							step="any"
							bind:value={policyScaleDown}
							placeholder="0.2"
							class="text-sm"
							data-testid="autoscale-down"
						/>
					</label>
				</div>
			{/if}

			<label class="block space-y-1">
				<span class="text-sm text-muted-foreground"
					>Residency zone <span class="text-muted-foreground/60">(optional)</span></span
				>
				<Input bind:value={policyZone} placeholder="eu-central" class="text-sm" />
			</label>

			<label class="block space-y-1">
				<span class="text-sm text-muted-foreground"
					>Cooldown <span class="text-muted-foreground/60">(seconds, optional)</span></span
				>
				<Input type="number" min="0" bind:value={policyCooldown} placeholder="60" class="text-sm" />
			</label>

			<label class="flex items-center gap-2 text-sm">
				<input type="checkbox" bind:checked={policyDedicated} data-testid="autoscale-dedicated" />
				<span class="text-muted-foreground">Dedicated single-model job</span>
			</label>
		</div>
		<Dialog.Footer>
			<Button variant="ghost" size="sm" class="text-sm" onclick={() => (policyFor = null)}>Cancel</Button>
			<Button
				size="sm"
				class="text-sm"
				disabled={policySaving || !policyNodePool.trim()}
				data-testid="autoscale-save"
				onclick={savePolicy}
			>
				{policySaving ? 'Saving…' : 'Save'}
			</Button>
		</Dialog.Footer>
	</Dialog.Content>
</Dialog.Root>
