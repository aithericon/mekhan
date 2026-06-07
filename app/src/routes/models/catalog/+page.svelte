<script lang="ts">
	// CATALOG tab — the model browser as a real page (was a modal). Discover
	// models from the OFFICIAL upstream catalogs and add them to the pool:
	//   Ollama Library — scraped from ollama.com; Add to pool pulls the slug.
	//   Hugging Face   — the HF JSON API (the vLLM source); on an Ollama runner a
	//     GGUF repo pulls via hf.co/<id>, on vLLM it's informational (Copy id).
	// With no model-server runner this is discovery-only — browse + copy ids, but
	// Add to pool is disabled until a runner exists. Add to pool CURATES the model
	// into the workspace SET (POST /api/v1/models) then publishes a Pull
	// ModelCommand to the chosen runner (control plane only, never inference).
	import { Tabs, TabsList, TabsTrigger } from '$lib/components/ui/tabs';
	import { Input } from '$lib/components/ui/input';
	import { Button } from '$lib/components/ui/button';
	import Search from '@lucide/svelte/icons/search';
	import Download from '@lucide/svelte/icons/download';
	import Copy from '@lucide/svelte/icons/copy';
	import ExternalLink from '@lucide/svelte/icons/external-link';
	import {
		listModelCatalog,
		listRunnerPresence,
		publishModelCommand,
		createModel,
		baseCommand,
		apiErrorMessage,
		type CatalogModel,
		type CatalogSource,
		type RunnerPresenceSnapshot
	} from '$lib/api/models';
	import { shortId } from '$lib/components/fleet/model-pool';
	import RunnerTargetPicker, {
		runnerAdvertises
	} from '$lib/components/fleet/RunnerTargetPicker.svelte';
	import { toast } from 'svelte-sonner';

	let source = $state<CatalogSource>('ollama');
	let query = $state('');
	let models = $state<CatalogModel[]>([]);
	let loading = $state(false);
	let error = $state<string | null>(null);
	let copied = $state<string | null>(null);
	let notice = $state<string | null>(null);
	let busy = $state<string | null>(null);

	// Provision targets — live PRESENT model-server runners, polled by the picker.
	// We mirror the presence snapshot here so the page can reason about the
	// SELECTED runner's advertised backends (vLLM-vs-Ollama gating below).
	let runners = $state<RunnerPresenceSnapshot[]>([]);
	let target = $state<string | null>(null);
	const selectedRunner = $derived(runners.find((r) => r.runner_id === target));

	async function loadRunners() {
		try {
			const all = await listRunnerPresence();
			runners = all.filter((r) => r.present === true);
			if (target !== null && !runners.some((r) => r.runner_id === target)) {
				target = runners[0]?.runner_id ?? null;
			}
		} catch {
			/* leave the picker as-is on a transient error */
		}
	}

	/** An hf.co/… pull only works on an Ollama runner — vLLM fixes its base at
	 *  launch and cannot pull a GGUF repo. Gate Add-to-pool accordingly. */
	const isHfId = (m: CatalogModel) => provisionId(m).startsWith('hf.co/');
	const hfNeedsOllama = $derived(
		(m: CatalogModel) => isHfId(m) && !runnerAdvertises(selectedRunner, 'ollama')
	);
	const canAdd = $derived(target !== null && busy === null);
	function addDisabledReason(m: CatalogModel): string | null {
		if (target === null) return 'Enrol a model-server runner to add to the pool';
		if (hfNeedsOllama(m)) return 'Selected runner lacks the ollama backend (vLLM base is fixed at launch)';
		return null;
	}

	// Debounced catalog fetch on (source, query).
	let timer: ReturnType<typeof setTimeout> | undefined;
	$effect(() => {
		const s = source;
		const q = query;
		clearTimeout(timer);
		timer = setTimeout(() => void fetchCatalog(s, q), 350);
		return () => clearTimeout(timer);
	});

	$effect(() => {
		void loadRunners();
		const t = setInterval(() => void loadRunners(), 5000);
		return () => clearInterval(t);
	});

	async function fetchCatalog(s: CatalogSource, q: string) {
		loading = true;
		error = null;
		try {
			const resp = await listModelCatalog(s, q.trim() || undefined);
			models = resp.models;
			error = resp.error ?? null;
		} catch (e) {
			models = [];
			error = e instanceof Error ? e.message : 'Catalog fetch failed';
		} finally {
			loading = false;
		}
	}

	/** The id to hand the runner. HF ids prefix hf.co/ so an Ollama runner can
	 *  pull the GGUF repo; the Ollama slug is used verbatim. */
	const provisionId = (m: CatalogModel) => (m.source === 'huggingface' ? `hf.co/${m.id}` : m.id);

	async function addToPool(m: CatalogModel) {
		if (!target) return;
		const id = provisionId(m);
		const runnerId = target;
		busy = id;
		notice = null;
		error = null;
		try {
			// Curate the model into the workspace SET first. A 409 means it is
			// already curated — benign, keep going to the pull.
			try {
				await createModel({ model_id: id, base: null, registry_resource_id: null, note: null });
			} catch (e) {
				if (!(e instanceof Error && /^API error 409:/.test(e.message))) throw e;
			}
			// Then publish the Pull command onto the chosen runner.
			await publishModelCommand(runnerId, baseCommand('pull', id));
			const msg = `Added ${id} to the pool — pulling onto runner ${shortId(runnerId)}; it will appear under Set as approved/loading.`;
			notice = msg;
			toast.success(msg);
		} catch (e) {
			toast.error(apiErrorMessage(e));
		} finally {
			busy = null;
		}
	}

	async function copyId(m: CatalogModel) {
		try {
			await navigator.clipboard.writeText(m.id);
			copied = m.id;
			setTimeout(() => (copied = copied === m.id ? null : copied), 1200);
		} catch {
			/* clipboard unavailable — ignore */
		}
	}
</script>

<div class="space-y-4" data-testid="models-catalog">
	<div class="flex flex-wrap items-baseline gap-3">
		<h2 class="text-base font-semibold tracking-tight text-foreground">Catalog</h2>
		<span class="text-sm text-muted-foreground">browse official sources, add onto a runner</span>
	</div>

	<!-- Add-to-pool target. No runner ⇒ discovery-only (browse + Copy id). -->
	{#if runners.length === 0}
		<p class="rounded-md border border-border/60 bg-muted/40 px-3 py-2 text-sm text-muted-foreground">
			No model-server runner enrolled — browse + copy ids here, then enrol a runner with a
			<code>[model_agent]</code> backend to add models to the pool.
		</p>
	{:else}
		<label class="flex items-center gap-2 text-sm text-muted-foreground">
			Add onto
			<RunnerTargetPicker
				value={target}
				onChange={(id) => (target = id)}
				requireBackend="ollama"
			/>
		</label>
	{/if}

	{#if notice}
		<div
			class="rounded-lg border border-emerald-200 bg-emerald-50 px-3 py-2 text-sm text-emerald-800 dark:border-emerald-800/50 dark:bg-emerald-950/40 dark:text-emerald-200"
		>
			{notice}
		</div>
	{/if}

	<Tabs bind:value={source}>
		<TabsList class="grid w-full max-w-sm grid-cols-2">
			<TabsTrigger value="ollama">Ollama Library</TabsTrigger>
			<TabsTrigger value="huggingface">Hugging Face</TabsTrigger>
		</TabsList>

		<div class="relative mt-3 max-w-md">
			<Search
				class="pointer-events-none absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground"
			/>
			<Input
				class="pl-9"
				placeholder={source === 'ollama'
					? 'Search ollama.com (e.g. llama, qwen)…'
					: 'Search Hugging Face (text-generation)…'}
				bind:value={query}
			/>
		</div>

		{#if source === 'huggingface'}
			<p class="mt-2 text-sm text-muted-foreground/80">
				vLLM fixes its base at engine launch, so on a vLLM node these are informational — use
				<span class="font-medium">Copy id</span> for config. An Ollama node can pull a GGUF repo
				(<code>hf.co/…</code>) directly.
			</p>
		{/if}

		<div class="mt-3">
			{#if error}
				<div
					class="rounded-lg border border-amber-200 bg-amber-50 px-3 py-2 text-sm text-amber-800 dark:border-amber-800/50 dark:bg-amber-950/40 dark:text-amber-200"
				>
					{error}
				</div>
			{/if}
			{#if loading && models.length === 0}
				<p class="py-10 text-center text-sm text-muted-foreground">Loading catalog…</p>
			{:else if models.length === 0}
				<p class="py-10 text-center text-sm text-muted-foreground">
					No models{query ? ` matching “${query}”` : ''}.
				</p>
			{:else}
				<ul class="grid gap-1.5 sm:grid-cols-2">
					{#each models as m (m.id)}
						{@const disabledReason = addDisabledReason(m)}
						<li
							class="flex items-center justify-between gap-3 rounded-lg border border-border/60 bg-card p-2.5"
						>
							<div class="min-w-0">
								<div class="flex items-center gap-2">
									<span class="truncate font-medium text-foreground">{m.name}</span>
									{#if m.url}
										<a
											href={m.url}
											target="_blank"
											rel="noopener noreferrer"
											class="text-muted-foreground/60 hover:text-foreground"
											title="Open upstream page"
										>
											<ExternalLink class="size-3.5" />
										</a>
									{/if}
								</div>
								<div class="mt-0.5 flex flex-wrap items-center gap-1.5 text-sm text-muted-foreground">
									{#if m.pulls}<span>↓ {m.pulls}</span>{/if}
									{#each (m.sizes ?? []).slice(0, 6) as s}
										<span class="rounded bg-muted px-1 py-px font-mono">{s}</span>
									{/each}
									{#each (m.capabilities ?? []).slice(0, 3) as c}
										<span class="rounded bg-muted/60 px-1 py-px">{c}</span>
									{/each}
								</div>
							</div>
							<div class="flex shrink-0 items-center gap-1.5">
								{#if m.source === 'huggingface'}
									<Button
										variant="ghost"
										size="sm"
										class="h-7 gap-1 px-2 text-sm"
										onclick={() => copyId(m)}
									>
										<Copy class="size-3.5" />
										{copied === m.id ? 'Copied' : 'Copy id'}
									</Button>
								{/if}
								<Button
									variant="outline"
									size="sm"
									class="h-7 gap-1 px-2 text-sm"
									disabled={!canAdd || disabledReason !== null}
									title={disabledReason ?? 'Curate into the pool + pull onto the selected runner'}
									onclick={() => addToPool(m)}
								>
									<Download class="size-3.5" />
									{busy === provisionId(m) ? '…' : 'Add to pool'}
								</Button>
							</div>
						</li>
					{/each}
				</ul>
			{/if}
		</div>
	</Tabs>
</div>
