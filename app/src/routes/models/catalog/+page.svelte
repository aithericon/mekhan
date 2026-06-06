<script lang="ts">
	// CATALOG tab — the model browser as a real page (was a modal). Discover
	// models from the OFFICIAL upstream catalogs and provision them onto a runner:
	//   Ollama Library — scraped from ollama.com; Provision pulls the slug.
	//   Hugging Face   — the HF JSON API (the vLLM source); on an Ollama runner a
	//     GGUF repo pulls via hf.co/<id>, on vLLM it's informational (Copy id).
	// With no model-server runner this is discovery-only — browse + copy ids, but
	// Provision is disabled until a runner exists. Provision publishes a Pull
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
		listFleetEngines,
		publishModelCommand,
		baseCommand,
		type CatalogModel,
		type CatalogSource
	} from '$lib/api/models';
	import { shortId } from '$lib/components/fleet/model-pool';

	let source = $state<CatalogSource>('ollama');
	let query = $state('');
	let models = $state<CatalogModel[]>([]);
	let loading = $state(false);
	let error = $state<string | null>(null);
	let copied = $state<string | null>(null);
	let notice = $state<string | null>(null);
	let busy = $state<string | null>(null);

	// Provision targets — live model-server nodes. Refreshed on mount + after a
	// provision so a freshly-enrolled runner appears in the picker.
	let runners = $state<{ id: string; label: string }[]>([]);
	let target = $state<string | null>(null);
	const canProvision = $derived(target !== null && busy === null);

	async function loadRunners() {
		try {
			const r = await listFleetEngines();
			runners = r.nodes.map((n) => ({ id: n.runner_id, label: `runner ${shortId(n.runner_id)}` }));
			if (target === null || !runners.some((x) => x.id === target)) {
				target = runners[0]?.id ?? null;
			}
		} catch {
			/* leave the picker as-is on a transient error */
		}
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

	async function provision(m: CatalogModel) {
		if (!target) return;
		const id = provisionId(m);
		const runnerId = target;
		busy = id;
		notice = null;
		try {
			await publishModelCommand(runnerId, baseCommand('pull', id));
			notice = `Provisioning ${id} onto runner ${shortId(runnerId)} — it will appear under Engines › ready to load when the download finishes.`;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Provision failed';
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
		<span class="text-sm text-muted-foreground">browse official sources, provision onto a runner</span>
	</div>

	<!-- Provision target. No runner ⇒ discovery-only (browse + Copy id). -->
	{#if runners.length === 0}
		<p class="rounded-md border border-border/60 bg-muted/40 px-3 py-2 text-sm text-muted-foreground">
			No model-server runner enrolled — browse + copy ids here, then enrol a runner with a
			<code>[model_agent]</code> backend to provision.
		</p>
	{:else}
		<label class="flex items-center gap-2 text-sm text-muted-foreground">
			Provision onto
			<select
				class="h-7 rounded-md border border-border/60 bg-background px-2 text-sm text-foreground"
				bind:value={target}
			>
				{#each runners as r (r.id)}
					<option value={r.id}>{r.label}</option>
				{/each}
			</select>
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
									disabled={!canProvision}
									title={canProvision
										? 'Pull onto the selected runner'
										: 'Enrol a model-server runner to provision'}
									onclick={() => provision(m)}
								>
									<Download class="size-3.5" />
									{busy === provisionId(m) ? '…' : 'Provision'}
								</Button>
							</div>
						</li>
					{/each}
				</ul>
			{/if}
		</div>
	</Tabs>
</div>
