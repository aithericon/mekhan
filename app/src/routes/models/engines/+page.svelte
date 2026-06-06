<script lang="ts">
	// ENGINES tab — the live per-node engine inventory (GET /api/v1/fleet/engines):
	// resident base engines (with C + headroom + loaded adapters), the
	// provisioned-to-disk "ready to load" set, and per-runner load / unload / pull
	// actions. Each action publishes a ModelCommand to the runner's model agent
	// (vLLM admin / Ollama Metal runtime) — control plane only, never inference.
	import { Button } from '$lib/components/ui/button';
	import Cpu from '@lucide/svelte/icons/cpu';
	import LibraryBig from '@lucide/svelte/icons/library-big';
	import {
		listFleetEngines,
		publishModelCommand,
		baseCommand,
		type FleetEnginesResponse
	} from '$lib/api/models';
	import { shortId } from '$lib/components/fleet/model-pool';

	let engines = $state<FleetEnginesResponse>({ headroom_from_router: false, nodes: [] });
	let error = $state<string | null>(null);
	let busy = $state<string | null>(null);
	let loadInputs = $state<Record<string, string>>({});

	async function poll() {
		try {
			engines = await listFleetEngines();
			error = null;
		} catch (err) {
			error = err instanceof Error ? err.message : 'Failed to load the engine inventory';
		}
	}

	$effect(() => {
		void poll();
		const t = setInterval(() => void poll(), 5000);
		return () => clearInterval(t);
	});

	async function act(runnerId: string, verb: 'load' | 'unload' | 'pull', modelId: string) {
		if (!modelId) return;
		busy = `${runnerId}:${modelId}:${verb}`;
		try {
			await publishModelCommand(runnerId, baseCommand(verb, modelId));
			// Fire-and-forget: give the agent a moment to apply + re-publish its
			// catalog. A pull downloads weights (slow); the next 5s poll surfaces it.
			await new Promise((r) => setTimeout(r, verb === 'pull' ? 800 : 1500));
			await poll();
		} catch (err) {
			error = err instanceof Error ? err.message : 'Command failed';
		} finally {
			busy = null;
		}
	}
</script>

<div class="space-y-4" data-testid="models-engines">
	<div class="flex items-baseline gap-3">
		<h2 class="text-sm font-semibold tracking-tight text-foreground">Engines</h2>
		<span class="text-sm text-muted-foreground">live per-node inventory</span>
		{#if !engines.headroom_from_router}
			<span class="text-xs text-muted-foreground/70">
				headroom = full budget (router poll unconfigured)
			</span>
		{/if}
		<Button
			variant="outline"
			size="sm"
			href="/models/catalog"
			class="ml-auto h-7 gap-1.5 self-center px-2.5 text-xs"
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
					<div class="mb-2 flex items-center justify-between">
						<span class="font-mono text-xs text-muted-foreground">runner {shortId(node.runner_id)}</span>
						<span class="text-xs text-muted-foreground">{node.engines.length} engine(s)</span>
					</div>

					{#if node.engines.length === 0}
						<p class="text-xs text-muted-foreground/70">no models resident</p>
					{:else}
						<ul class="space-y-1.5">
							{#each node.engines as e (e.base)}
								<li class="flex items-center justify-between gap-2 text-sm">
									<span class="flex items-baseline gap-2 truncate">
										<span class="truncate font-medium text-foreground">{e.base}</span>
										<span class="shrink-0 text-xs text-muted-foreground">
											C {e.max_num_seqs ?? '–'} · headroom {e.headroom ?? '–'}
										</span>
									</span>
									<Button
										variant="ghost"
										size="sm"
										class="h-6 shrink-0 px-2 text-xs"
										disabled={busy !== null}
										onclick={() => act(node.runner_id, 'unload', e.base)}
									>
										{busy === `${node.runner_id}:${e.base}:unload` ? '…' : 'Unload'}
									</Button>
								</li>
								{#if e.loaded_adapters.length > 0}
									<li class="pl-3 text-xs text-muted-foreground">
										adapters: {e.loaded_adapters.map((a) => a.model_id).join(', ')}
									</li>
								{/if}
							{/each}
						</ul>
					{/if}

					<!-- Provisioned to disk, NOT resident — one click to load (no re-download). -->
					{#if (node.pulled ?? []).length > 0}
						<ul class="mt-2 space-y-1 border-t border-border/40 pt-2">
							<li class="text-xs font-medium text-muted-foreground/70">ready to load</li>
							{#each node.pulled ?? [] as p (p)}
								<li class="flex items-center justify-between gap-2 text-sm">
									<span class="truncate text-muted-foreground">{p}</span>
									<Button
										variant="ghost"
										size="sm"
										class="h-6 shrink-0 px-2 text-xs"
										disabled={busy !== null}
										onclick={() => act(node.runner_id, 'load', p)}
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
							class="h-7 min-w-0 flex-1 rounded-md border border-border/60 bg-background px-2 text-xs"
							placeholder="model id (e.g. llama3.2:1b)"
							bind:value={loadInputs[node.runner_id]}
						/>
						<Button
							variant="ghost"
							size="sm"
							class="h-7 shrink-0 px-2 text-xs"
							disabled={busy !== null || !loadInputs[node.runner_id]}
							onclick={() => act(node.runner_id, 'pull', loadInputs[node.runner_id] ?? '')}
							title="Provision (download) to disk without loading"
						>
							Pull
						</Button>
						<Button
							variant="outline"
							size="sm"
							class="h-7 shrink-0 px-2 text-xs"
							disabled={busy !== null || !loadInputs[node.runner_id]}
							onclick={() => act(node.runner_id, 'load', loadInputs[node.runner_id] ?? '')}
						>
							Load
						</Button>
					</div>
				</div>
			{/each}
		</div>
	{/if}
</div>
