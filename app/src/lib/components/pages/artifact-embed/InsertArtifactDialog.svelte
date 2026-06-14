<!--
  InsertArtifactDialog — the "Insert run media" browser for the Report editor.

  Shows the run's renderable artifacts grouped by render bucket (gp-posterior /
  images / videos / …) with thumbnails, so it's evident what's there. From here
  you can:
    - click a tile          → embed THAT artifact (pinned snapshot),
    - "Embed group"         → embed the whole bucket as a live, scrubbable panel,
    - "Embed all media"     → embed every renderable artifact as one live panel.

  Reuses the same per-process live store + renderer registry as the Process
  Overview, so the picker and the embedded block agree on what counts as media.
-->
<script lang="ts">
	import * as Dialog from '$lib/components/ui/dialog';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import type { Editor } from '@tiptap/core';
	import type { LiveArtifactEntry } from '$lib/api/client';
	import {
		isShowcaseEntry,
		groupKey,
		groupLabel,
		stepNumber
	} from '$lib/components/process-live/renderers/registry';
	import ArtifactThumb from './ArtifactThumb.svelte';
	import type { ArtifactEmbedContext } from './embed-context';
	import type { ArtifactEmbedAttrs } from './artifact-embed';

	const MAX_TILES = 8;

	let {
		open = $bindable(),
		editor,
		context
	}: {
		open: boolean;
		editor: Editor | null;
		context: ArtifactEmbedContext;
	} = $props();

	let selectedProcessId = $state<string>('');

	// Default to the first process that has media (fall back to the first) once
	// the dialog opens. Resolving a store inits its fetch + SSE; stores are
	// memoized in the context so this is cheap to repeat.
	$effect(() => {
		if (!open) return;
		const procs = context.processes;
		if (!procs.length) return;
		if (selectedProcessId && procs.some((p) => p.id === selectedProcessId)) return;
		const withMedia = procs.find((p) => countFor(p.id) > 0);
		selectedProcessId = (withMedia ?? procs[0]).id;
	});

	function countFor(processId: string): number {
		return context.getArtifactStore(processId).artifacts.filter(isShowcaseEntry).length;
	}

	const store = $derived(selectedProcessId ? context.getArtifactStore(selectedProcessId) : null);
	const showcase = $derived(store ? store.artifacts.filter(isShowcaseEntry) : []);
	const loading = $derived(!!store && showcase.length === 0 && store.artifactStatus === 'loading');

	interface Group {
		key: string;
		label: string;
		entries: LiveArtifactEntry[];
	}
	const groups = $derived.by<Group[]>(() => {
		const m = new Map<string, LiveArtifactEntry[]>();
		for (const e of showcase) {
			const k = groupKey(e);
			const arr = m.get(k) ?? [];
			arr.push(e);
			m.set(k, arr);
		}
		const out: Group[] = [];
		for (const [k, arr] of m) {
			arr.sort((a, b) => {
				const s = stepNumber(a) - stepNumber(b);
				return s !== 0 ? s : new Date(a.created_at).getTime() - new Date(b.created_at).getTime();
			});
			out.push({ key: k, label: groupLabel(k), entries: arr });
		}
		const rank = (k: string) => (k.startsWith('hint:') ? 0 : k.startsWith('mime:') ? 1 : 2);
		out.sort((a, b) => rank(a.key) - rank(b.key) || a.label.localeCompare(b.label));
		return out;
	});

	function insertNode(partial: Partial<ArtifactEmbedAttrs>) {
		if (!editor || !selectedProcessId) return;
		const proc = context.processes.find((p) => p.id === selectedProcessId);
		const attrs: ArtifactEmbedAttrs = {
			processId: selectedProcessId,
			processName: proc?.name ?? '',
			mode: 'all',
			groupKey: '',
			groupLabel: '',
			artifactId: '',
			artifactName: '',
			storagePath: '',
			mimeType: '',
			renderHint: '',
			category: '',
			processStep: '',
			caption: '',
			...partial
		};
		// Insert at the END of the doc (+ a trailing paragraph) rather than at the
		// current selection. The editor loses a valid text selection once it ends
		// on an atom block (ProseMirror "TextSelection endpoint not pointing into a
		// node with inline content"), which would make a selection-relative
		// insertContent silently no-op for the 2nd+ block.
		const end = editor.state.doc.content.size;
		editor
			.chain()
			.insertContentAt(end, [{ type: 'artifactEmbed', attrs }, { type: 'paragraph' }])
			.focus('end')
			.run();
		open = false;
	}

	function embedArtifact(e: LiveArtifactEntry) {
		const hint = typeof e.user_metadata?.render_hint === 'string' ? e.user_metadata.render_hint : '';
		insertNode({
			mode: 'artifact',
			artifactId: e.artifact_id ?? e.id ?? '',
			artifactName: e.name ?? e.filename ?? '',
			storagePath: e.storage_path ?? '',
			mimeType: e.mime_type ?? '',
			renderHint: hint,
			category: e.category ?? '',
			processStep: e.process_step ?? ''
		});
	}
</script>

<Dialog.Root bind:open>
	<Dialog.Content class="flex max-h-[80vh] flex-col sm:max-w-2xl">
		<Dialog.Header>
			<Dialog.Title>Embed run media</Dialog.Title>
			<Dialog.Description>
				Click a tile to embed that artifact, embed a whole group, or drop in a live panel
				of everything this run produces.
			</Dialog.Description>
		</Dialog.Header>

		{#if context.processes.length > 1}
			<div class="flex flex-col gap-1.5">
				<label class="text-sm font-medium" for="embed-process">Process</label>
				<select
					id="embed-process"
					bind:value={selectedProcessId}
					class="border-input bg-background focus-visible:ring-ring h-9 rounded-md border px-3 text-sm focus-visible:outline-none focus-visible:ring-2"
				>
					{#each context.processes as p (p.id)}
						<option value={p.id}>{p.name} ({countFor(p.id)})</option>
					{/each}
				</select>
			</div>
		{/if}

		<div class="-mx-1 min-h-0 flex-1 overflow-y-auto px-1 py-2">
			{#if loading}
				<p class="py-8 text-center text-sm text-muted-foreground">Loading media…</p>
			{:else if groups.length === 0}
				<p class="py-8 text-center text-sm text-muted-foreground">
					This run hasn't produced renderable media yet. You can still drop in a live panel
					below — it'll fill in as artifacts arrive.
				</p>
			{:else}
				<div class="flex flex-col gap-5">
					{#each groups as g (g.key)}
						<section class="flex flex-col gap-2">
							<div class="flex items-center justify-between gap-2">
								<div class="flex items-center gap-2">
									<Badge variant="secondary" class="font-mono text-xs">{g.label}</Badge>
									<span class="text-xs text-muted-foreground">
										{g.entries.length} item{g.entries.length === 1 ? '' : 's'}
									</span>
								</div>
								<Button variant="outline" size="sm" onclick={() => insertNode({ mode: 'group', groupKey: g.key, groupLabel: g.label })}>
									Embed group
								</Button>
							</div>
							<div class="grid grid-cols-4 gap-2">
								{#each g.entries.slice(0, MAX_TILES) as e (e.artifact_id ?? e.id)}
									<button
										type="button"
										class="group flex flex-col gap-1 text-left"
										title={`Embed ${e.name}`}
										onclick={() => embedArtifact(e)}
									>
										<div class="ring-offset-background transition group-hover:ring-2 group-hover:ring-ring group-hover:ring-offset-1 rounded-md">
											<ArtifactThumb entry={e} />
										</div>
										<span class="truncate text-xs text-muted-foreground">{e.name}</span>
									</button>
								{/each}
							</div>
							{#if g.entries.length > MAX_TILES}
								<span class="text-xs text-muted-foreground">
									+{g.entries.length - MAX_TILES} more — use “Embed group” for all of them.
								</span>
							{/if}
						</section>
					{/each}
				</div>
			{/if}
		</div>

		<Dialog.Footer class="gap-2">
			<Button variant="ghost" onclick={() => (open = false)}>Cancel</Button>
			<Button onclick={() => insertNode({ mode: 'all' })} disabled={!selectedProcessId}>
				Embed all media (live)
			</Button>
		</Dialog.Footer>
	</Dialog.Content>
</Dialog.Root>
