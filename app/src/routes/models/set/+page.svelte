<script lang="ts">
	// SET tab — the operator-curated model set (GET /api/v1/models): every model
	// approved into the pool, each decorated with its lifecycle `state` and the
	// `available` AND-gate (state == loaded AND a live runner advertises it — the
	// flag the editor model picker filters on). Lifecycle moves go through
	// POST /api/v1/models/{id}/transition (validated against legal_transitions;
	// an illegal edge → 409, surfaced here).
	import { Button } from '$lib/components/ui/button';
	import Boxes from '@lucide/svelte/icons/boxes';
	import {
		listLoadedModels,
		transitionModel,
		type ModelSetView,
		type ModelState
	} from '$lib/api/models';
	import { statusTone } from '$lib/components/fleet/model-pool';

	const STATES: ModelState[] = ['approved', 'loading', 'loaded', 'draining', 'unloaded'];

	let models = $state<ModelSetView[]>([]);
	let error = $state<string | null>(null);
	let busy = $state<string | null>(null);
	// Per-model pending transition target (the <select> value).
	let pending = $state<Record<string, ModelState>>({});

	async function poll() {
		try {
			models = await listLoadedModels();
			error = null;
		} catch (err) {
			error = err instanceof Error ? err.message : 'Failed to load the model set';
		}
	}

	$effect(() => {
		void poll();
		const t = setInterval(() => void poll(), 5000);
		return () => clearInterval(t);
	});

	async function applyTransition(m: ModelSetView) {
		const target = pending[m.model_id];
		if (!target || target === m.state) return;
		busy = m.model_id;
		try {
			await transitionModel(m.model_id, target);
			delete pending[m.model_id];
			await poll();
		} catch (err) {
			// Illegal edge → 409, surfaced verbatim.
			error = err instanceof Error ? err.message : 'Transition failed';
		} finally {
			busy = null;
		}
	}
</script>

<div class="space-y-4" data-testid="models-set">
	<div class="flex items-baseline gap-3">
		<h2 class="text-sm font-semibold tracking-tight text-foreground">Curated model set</h2>
		<span class="text-sm text-muted-foreground">approved into the pool · live-runner AND-gate</span>
	</div>

	{#if error}
		<div
			class="rounded-lg border border-amber-200 bg-amber-50 px-4 py-2 text-sm text-amber-800 dark:border-amber-800/50 dark:bg-amber-950/40 dark:text-amber-200"
		>
			{error}
		</div>
	{/if}

	{#if models.length === 0}
		<div
			class="flex flex-col items-center gap-2 rounded-lg border border-dashed border-border/60 py-10 text-sm text-muted-foreground"
		>
			<Boxes class="size-8 text-muted-foreground/40" />
			No curated models. Add a <code>model_registry</code> resource to approve a model into the pool.
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
						<span class="ml-auto text-xs {statusTone(String(m.state))}">{m.state}</span>
					</div>
					<div class="mt-0.5 pl-3.5 text-xs text-muted-foreground">
						{#if m.base}LoRA of {m.base} · {/if}replicas {m.replicas}
						{#if m.note}· {m.note}{/if}
					</div>

					<!-- Lifecycle transition: pick a target state + Apply. Illegal edges
						 are rejected by the server (409) and surfaced above. -->
					<div class="mt-2 flex items-center gap-1.5 border-t border-border/40 pt-2">
						<select
							class="h-7 min-w-0 flex-1 rounded-md border border-border/60 bg-background px-2 text-xs text-foreground"
							value={pending[m.model_id] ?? m.state}
							onchange={(e) =>
								(pending[m.model_id] = e.currentTarget.value as ModelState)}
						>
							{#each STATES as s}
								<option value={s}>{s}</option>
							{/each}
						</select>
						<Button
							variant="outline"
							size="sm"
							class="h-7 shrink-0 px-2 text-xs"
							disabled={busy !== null ||
								!pending[m.model_id] ||
								pending[m.model_id] === m.state}
							onclick={() => applyTransition(m)}
						>
							{busy === m.model_id ? '…' : 'Apply'}
						</Button>
					</div>
				</div>
			{/each}
		</div>
	{/if}
</div>
