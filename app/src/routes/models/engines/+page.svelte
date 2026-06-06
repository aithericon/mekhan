<script lang="ts">
	// ENGINES tab — the live per-node engine inventory (GET /api/v1/fleet/engines):
	// resident base engines (with C + headroom + loaded adapters), the
	// provisioned-to-disk "ready to load" set, and per-runner load / unload / pull
	// actions. Load / unload now go through the UNIFIED operator endpoints
	// (loadModel / unloadModel) so acting here also CURATES the model into the SET
	// and drives its lifecycle state — not just a raw runtime command. Pull stays a
	// plain runtime command (provision-to-disk, no curation). All control plane,
	// never inference.
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import * as Dialog from '$lib/components/ui/dialog';
	import Cpu from '@lucide/svelte/icons/cpu';
	import LibraryBig from '@lucide/svelte/icons/library-big';
	import { toast } from 'svelte-sonner';
	import {
		listFleetEngines,
		listRunnerPresence,
		loadModel,
		unloadModel,
		publishModelCommand,
		baseCommand,
		apiErrorMessage,
		type FleetEnginesResponse,
		type RunnerPresenceSnapshot
	} from '$lib/api/models';
	import { shortId } from '$lib/components/fleet/model-pool';

	let engines = $state<FleetEnginesResponse>({ headroom_from_router: false, nodes: [] });
	// Per-poll presence cache, keyed by runner_id, so we can decorate each engine
	// card with a friendly "{name} · [{backends}]" label instead of the opaque
	// short id. Fail-soft: a presence fetch error leaves the map empty and we fall
	// back to the short id.
	let presence = $state<Record<string, RunnerPresenceSnapshot>>({});
	let error = $state<string | null>(null);
	let busy = $state<string | null>(null);
	let loadInputs = $state<Record<string, string>>({});

	// Pending unload confirmation — populated when the operator clicks Unload, read
	// by the confirmation dialog, cleared on confirm / cancel.
	let unloadTarget = $state<{ runnerId: string; base: string; runnerLabel: string } | null>(null);

	async function poll() {
		// Fetch presence alongside the inventory; presence failing must NOT wipe the
		// engine board, so it's caught independently and folded fail-soft.
		const [inv, pres] = await Promise.allSettled([listFleetEngines(), listRunnerPresence()]);
		if (inv.status === 'fulfilled') {
			engines = inv.value;
			error = null;
		} else {
			error =
				inv.reason instanceof Error ? inv.reason.message : 'Failed to load the engine inventory';
		}
		if (pres.status === 'fulfilled') {
			presence = Object.fromEntries(pres.value.map((r) => [r.runner_id, r]));
		}
	}

	$effect(() => {
		void poll();
		const t = setInterval(() => void poll(), 5000);
		return () => clearInterval(t);
	});

	/**
	 * Human-readable label for a runner: its presence-advertised short id +
	 * backends. Presence carries no display name, so the short id stands in as the
	 * name; backends render as a compact `[a, b]`. Falls back to the bare short id
	 * when presence has no row for this runner.
	 */
	function runnerName(runnerId: string): string {
		return shortId(runnerId);
	}

	function runnerBackends(runnerId: string): string[] {
		return presence[runnerId]?.backends ?? [];
	}

	function hasPresence(runnerId: string): boolean {
		return presence[runnerId] !== undefined;
	}

	// ── Actions ────────────────────────────────────────────────────────────────

	/** Unified load: curates `modelId` into the SET + drives it loading on `runnerId`. */
	async function load(runnerId: string, modelId: string) {
		if (!modelId) return;
		busy = `${runnerId}:${modelId}:load`;
		try {
			await loadModel(modelId, runnerId);
			// Fire-and-forget on the agent side: give it a moment to apply + re-publish
			// its catalog, then the next 5s poll surfaces the resident engine.
			await new Promise((r) => setTimeout(r, 1500));
			await poll();
		} catch (err) {
			toast.error(apiErrorMessage(err));
		} finally {
			busy = null;
		}
	}

	/** Provision-to-disk (no curation, no load) — stays a plain runtime command. */
	async function pull(runnerId: string, modelId: string) {
		if (!modelId) return;
		busy = `${runnerId}:${modelId}:pull`;
		try {
			await publishModelCommand(runnerId, baseCommand('pull', modelId));
			await new Promise((r) => setTimeout(r, 800));
			await poll();
		} catch (err) {
			toast.error(apiErrorMessage(err));
		} finally {
			busy = null;
		}
	}

	function askUnload(runnerId: string, base: string) {
		unloadTarget = { runnerId, base, runnerLabel: runnerName(runnerId) };
	}

	/** Unified unload (after confirmation): drains the SET row + evicts on the runner. */
	async function confirmUnload() {
		const t = unloadTarget;
		if (!t) return;
		unloadTarget = null;
		busy = `${t.runnerId}:${t.base}:unload`;
		try {
			await unloadModel(t.base, t.runnerId);
			await new Promise((r) => setTimeout(r, 1500));
			await poll();
		} catch (err) {
			toast.error(apiErrorMessage(err));
		} finally {
			busy = null;
		}
	}
</script>

<div class="space-y-4" data-testid="models-engines">
	<div class="flex items-baseline gap-3">
		<h2 class="text-base font-semibold tracking-tight text-foreground">Engines</h2>
		<span class="text-sm text-muted-foreground">live per-node inventory</span>
		{#if !engines.headroom_from_router}
			<span class="text-sm text-muted-foreground/70">
				headroom = full budget (router poll unconfigured)
			</span>
		{/if}
		<Button
			variant="outline"
			size="sm"
			href="/models/catalog"
			class="ml-auto h-7 gap-1.5 self-center px-2.5 text-sm"
		>
			<LibraryBig class="size-3.5" />
			Browse catalog
		</Button>
	</div>

	{#if error}
		<div
			class="rounded-lg border border-amber-200 bg-amber-50 px-4 py-2 text-sm text-amber-800 dark:border-amber-800/50 dark:bg-amber-950/40 dark:text-amber-200"
		>
			{error}
		</div>
	{/if}

	{#if engines.nodes.length === 0}
		<div
			class="flex flex-col items-center gap-2 rounded-lg border border-dashed border-border/60 py-10 text-sm text-muted-foreground"
		>
			<Cpu class="size-8 text-muted-foreground/40" />
			No model-server runners. Enrol a runner with a <code>[model_agent]</code> backend (vLLM or Ollama).
			<Button variant="outline" size="sm" href="/models/catalog" class="mt-1 gap-1.5">
				<LibraryBig class="size-4" />
				Browse the catalog
			</Button>
		</div>
	{:else}
		<div class="grid gap-3 sm:grid-cols-2">
			{#each engines.nodes as node (node.runner_id)}
				<div class="rounded-lg border border-border/60 bg-card p-3" data-testid="engine-card">
					<div class="mb-2 flex items-center justify-between gap-2">
						{#if hasPresence(node.runner_id)}
							<span class="flex min-w-0 items-center gap-1.5">
								<span class="truncate font-mono text-sm text-foreground"
									>{runnerName(node.runner_id)}</span
								>
								{#each runnerBackends(node.runner_id) as b (b)}
									<Badge variant="secondary" class="shrink-0 font-mono text-xs">{b}</Badge>
								{/each}
							</span>
						{:else}
							<span class="font-mono text-sm text-muted-foreground"
								>runner {shortId(node.runner_id)}</span
							>
						{/if}
						<span class="shrink-0 text-sm text-muted-foreground">{node.engines.length} engine(s)</span>
					</div>

					{#if node.engines.length === 0}
						<p class="text-sm text-muted-foreground/70">no models resident</p>
					{:else}
						<ul class="space-y-1.5">
							{#each node.engines as e (e.base)}
								<li class="flex items-center justify-between gap-2 text-sm">
									<span class="flex items-baseline gap-2 truncate">
										<span class="truncate font-medium text-foreground">{e.base}</span>
										{#if e.max_num_seqs != null}
											<span class="shrink-0 text-sm text-muted-foreground">
												C {e.max_num_seqs} · headroom {e.headroom ?? '–'}
											</span>
										{/if}
									</span>
									<Button
										variant="ghost"
										size="sm"
										class="h-6 shrink-0 px-2 text-sm"
										disabled={busy !== null}
										onclick={() => askUnload(node.runner_id, e.base)}
									>
										{busy === `${node.runner_id}:${e.base}:unload` ? '…' : 'Unload'}
									</Button>
								</li>
								{#if e.loaded_adapters.length > 0}
									<li class="pl-3 text-sm text-muted-foreground">
										adapters: {e.loaded_adapters.map((a) => a.model_id).join(', ')}
									</li>
								{/if}
							{/each}
						</ul>
					{/if}

					<!-- Provisioned to disk, NOT resident — one click to load (no re-download). -->
					{#if (node.pulled ?? []).length > 0}
						<ul class="mt-2 space-y-1 border-t border-border/40 pt-2">
							<li class="text-sm font-medium text-muted-foreground/70">ready to load</li>
							{#each node.pulled ?? [] as p (p)}
								<li class="flex items-center justify-between gap-2 text-sm">
									<span class="truncate text-muted-foreground">{p}</span>
									<Button
										variant="ghost"
										size="sm"
										class="h-6 shrink-0 px-2 text-sm"
										disabled={busy !== null}
										onclick={() => load(node.runner_id, p)}
									>
										{busy === `${node.runner_id}:${p}:load` ? '…' : 'Load'}
									</Button>
								</li>
							{/each}
						</ul>
					{/if}

					<!-- Provision / load a model by id, or browse the catalog. -->
					<div class="mt-2 flex items-center gap-1.5 border-t border-border/40 pt-2">
						<input
							class="h-7 min-w-0 flex-1 rounded-md border border-border/60 bg-background px-2 text-sm"
							placeholder="model id (e.g. llama3.2:1b)"
							bind:value={loadInputs[node.runner_id]}
						/>
						<Button
							variant="ghost"
							size="sm"
							class="h-7 shrink-0 px-2 text-sm"
							disabled={busy !== null || !loadInputs[node.runner_id]}
							onclick={() => pull(node.runner_id, loadInputs[node.runner_id] ?? '')}
							title="Provision (download) to disk without loading"
						>
							Pull
						</Button>
						<Button
							variant="outline"
							size="sm"
							class="h-7 shrink-0 px-2 text-sm"
							disabled={busy !== null || !loadInputs[node.runner_id]}
							onclick={() => load(node.runner_id, loadInputs[node.runner_id] ?? '')}
						>
							Load
						</Button>
					</div>
				</div>
			{/each}
		</div>
	{/if}
</div>

<!-- Unload confirmation — unified unload may interrupt in-flight inference. -->
<Dialog.Root
	open={unloadTarget !== null}
	onOpenChange={(o) => {
		if (!o) unloadTarget = null;
	}}
>
	<Dialog.Content class="max-w-md" data-testid="unload-confirm">
		<Dialog.Header>
			<Dialog.Title>Unload model</Dialog.Title>
			<Dialog.Description>
				{#if unloadTarget}
					Unload <span class="font-medium text-foreground">{unloadTarget.base}</span> from
					<span class="font-mono text-foreground">{unloadTarget.runnerLabel}</span>? In-flight requests
					may be interrupted.
				{/if}
			</Dialog.Description>
		</Dialog.Header>
		<Dialog.Footer>
			<Button variant="outline" size="sm" onclick={() => (unloadTarget = null)}>Cancel</Button>
			<Button
				variant="destructive"
				size="sm"
				data-testid="unload-confirm-btn"
				onclick={() => void confirmUnload()}
			>
				Unload
			</Button>
		</Dialog.Footer>
	</Dialog.Content>
</Dialog.Root>
