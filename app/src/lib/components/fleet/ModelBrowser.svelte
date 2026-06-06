<script lang="ts">
	// The model browser — discover models from the OFFICIAL upstream catalogs and
	// provision them onto a runner. Two tabs:
	//   Ollama Library — scraped from ollama.com; "Provision" pulls the slug onto
	//     the target runner (`ollama pull <slug>`), after which it appears in the
	//     engine card's "ready to load" list.
	//   Hugging Face   — the HF JSON API (the vLLM source). On an Ollama runner a
	//     GGUF repo can be pulled (`hf.co/<id>`); on vLLM it's informational (the
	//     base is fixed at engine launch), so "Copy id" is offered for config use.
	//
	// Backend-agnostic: the parent owns the actual command + busy/poll cycle and
	// passes `onprovision(provisionId)`. The browser only fetches catalog metadata
	// (cached server-side) and surfaces the two actions.
	import {
		Dialog,
		DialogContent,
		DialogHeader,
		DialogTitle,
		DialogDescription
	} from '$lib/components/ui/dialog';
	import { Tabs, TabsList, TabsTrigger } from '$lib/components/ui/tabs';
	import { Input } from '$lib/components/ui/input';
	import { Button } from '$lib/components/ui/button';
	import Search from '@lucide/svelte/icons/search';
	import Download from '@lucide/svelte/icons/download';
	import Copy from '@lucide/svelte/icons/copy';
	import ExternalLink from '@lucide/svelte/icons/external-link';
	import {
		listModelCatalog,
		type CatalogModel,
		type CatalogSource
	} from '$lib/api/models';

	let {
		open = $bindable(false),
		runnerLabel = '',
		onprovision
	}: {
		open?: boolean;
		runnerLabel?: string;
		/** Provision (pull) `provisionId` onto the target runner. */
		onprovision: (provisionId: string) => void;
	} = $props();

	let source = $state<CatalogSource>('ollama');
	let query = $state('');
	let models = $state<CatalogModel[]>([]);
	let loading = $state(false);
	let error = $state<string | null>(null);
	let copied = $state<string | null>(null);

	// Debounced fetch on (source, query). Re-runs whenever the modal is open and
	// either changes; a short delay keeps us from firing per keystroke.
	let timer: ReturnType<typeof setTimeout> | undefined;
	$effect(() => {
		// track deps
		const s = source;
		const q = query;
		if (!open) return;
		clearTimeout(timer);
		timer = setTimeout(() => void fetchCatalog(s, q), 350);
		return () => clearTimeout(timer);
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

	/** The id to hand the runner. HF ids prefix `hf.co/` so an Ollama runner can
	 *  pull the GGUF repo; the Ollama slug is used verbatim. */
	function provisionId(m: CatalogModel): string {
		return m.source === 'huggingface' ? `hf.co/${m.id}` : m.id;
	}

	function provision(m: CatalogModel) {
		onprovision(provisionId(m));
		open = false;
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

<Dialog bind:open>
	<DialogContent class="max-w-2xl">
		<DialogHeader>
			<DialogTitle>Browse models</DialogTitle>
			<DialogDescription>
				Provision a model from an official catalog{runnerLabel ? ` onto ${runnerLabel}` : ''}.
				Provisioning pulls the weights to the runner; load it once it's ready.
			</DialogDescription>
		</DialogHeader>

		<Tabs bind:value={source}>
			<TabsList class="grid w-full grid-cols-2">
				<TabsTrigger value="ollama">Ollama Library</TabsTrigger>
				<TabsTrigger value="huggingface">Hugging Face</TabsTrigger>
			</TabsList>

			<div class="relative mt-3">
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
				<p class="mt-2 text-xs text-muted-foreground/80">
					vLLM fixes its base at engine launch, so on a vLLM node these are informational —
					use <span class="font-medium">Copy id</span> for config. An Ollama node can pull a GGUF
					repo (<code>hf.co/…</code>) directly.
				</p>
			{/if}

			<!-- One results list — the active tab drives `source` (and the fetch),
				 so the list reflects whichever catalog is selected. -->
			<div class="mt-3">
				{#if error}
					<div
						class="rounded-lg border border-amber-200 bg-amber-50 px-3 py-2 text-sm text-amber-800 dark:border-amber-800/50 dark:bg-amber-950/40 dark:text-amber-200"
					>
						{error}
					</div>
				{/if}
				{#if loading && models.length === 0}
					<p class="py-8 text-center text-sm text-muted-foreground">Loading catalog…</p>
				{:else if models.length === 0}
					<p class="py-8 text-center text-sm text-muted-foreground">
						No models{query ? ` matching “${query}”` : ''}.
					</p>
				{:else}
					<ul class="max-h-[48vh] space-y-1.5 overflow-y-auto pr-1">
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
									<div class="mt-0.5 flex flex-wrap items-center gap-1.5 text-xs text-muted-foreground">
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
											class="h-7 gap-1 px-2 text-xs"
											onclick={() => copyId(m)}
										>
											<Copy class="size-3.5" />
											{copied === m.id ? 'Copied' : 'Copy id'}
										</Button>
									{/if}
									<Button
										variant="outline"
										size="sm"
										class="h-7 gap-1 px-2 text-xs"
										onclick={() => provision(m)}
									>
										<Download class="size-3.5" />
										Provision
									</Button>
								</div>
							</li>
						{/each}
					</ul>
				{/if}
			</div>
		</Tabs>
	</DialogContent>
</Dialog>
