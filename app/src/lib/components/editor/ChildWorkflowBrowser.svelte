<script lang="ts">
	import * as Dialog from '$lib/components/ui/dialog';
	import { Input } from '$lib/components/ui/input';
	import { Badge } from '$lib/components/ui/badge';
	import FolderTreeIcon from '@lucide/svelte/icons/folder-tree';
	import Lock from '@lucide/svelte/icons/lock';
	import Tag from '@lucide/svelte/icons/tag';
	import Search from '@lucide/svelte/icons/search';
	import ExternalLink from '@lucide/svelte/icons/external-link';
	import {
		listTemplates,
		listFolders,
		listWorkspaceTags,
		type Template,
		type Folder
	} from '$lib/api/client';
	import { workspaces } from '$lib/workspaces/store.svelte';
	import FolderTree from '$lib/components/FolderTree.svelte';
	import { familyId } from '$lib/editor/template-utils';

	interface Props {
		open: boolean;
		/** Parent family id — excluded from results and used to scope the
		 *  "Private to this workflow" group. */
		currentTemplateId?: string;
		onselect: (familyId: string) => void;
	}

	let { open = $bindable(), currentTemplateId, onselect }: Props = $props();

	let folders = $state<Folder[]>([]);
	let tags = $state<string[]>([]);
	let templates = $state<Template[]>([]);
	let loading = $state(false);
	let error = $state<string | null>(null);

	// `catalogue` = the workspace's public/shared templates (private hidden);
	// `private` = this workflow's own private children (drafts included).
	let mode = $state<'catalogue' | 'private'>('catalogue');
	let folderId = $state<string | null>(null);
	let tag = $state<string | null>(null);
	let search = $state('');
	let publishedOnly = $state(false);

	// Folders + tags load once per open.
	$effect(() => {
		if (!open) return;
		const ws = workspaces.active?.id;
		if (!ws) return;
		listFolders(ws)
			.then((f) => (folders = f))
			.catch(() => (folders = []));
		listWorkspaceTags(ws)
			.then((t) => (tags = t))
			.catch(() => (tags = []));
	});

	// Result set reloads when the active facet changes (not on each keystroke —
	// search filters the loaded page client-side for snappiness).
	$effect(() => {
		if (!open) return;
		const m = mode;
		const fid = folderId;
		const tg = tag;
		const pub = publishedOnly;
		const cur = currentTemplateId;
		let cancelled = false;
		loading = true;
		error = null;
		const req =
			m === 'private' && cur
				? listTemplates({ pageSize: 100, ownerTemplateId: cur })
				: listTemplates({
						pageSize: 100,
						published: pub || undefined,
						folderId: fid || undefined,
						tag: tg || undefined
					});
		req
			.then((res) => {
				if (cancelled) return;
				templates = (res.items ?? []).filter((t) => familyId(t) !== cur && t.id !== cur);
			})
			.catch((e) => {
				if (!cancelled) {
					error = String(e);
					templates = [];
				}
			})
			.finally(() => {
				if (!cancelled) loading = false;
			});
		return () => {
			cancelled = true;
		};
	});

	const filtered = $derived.by(() => {
		const q = search.trim().toLowerCase();
		if (!q) return templates;
		return templates.filter(
			(t) =>
				t.name.toLowerCase().includes(q) ||
				(t.description ?? '').toLowerCase().includes(q)
		);
	});

	function pick(t: Template) {
		onselect(familyId(t));
		open = false;
	}

	function openInTab(t: Template, e: MouseEvent) {
		e.stopPropagation();
		window.open(`/templates/${t.id}`, '_blank');
	}

	function selectFolder(id: string | null) {
		mode = 'catalogue';
		folderId = id;
		tag = null;
	}
	function selectPrivate() {
		mode = 'private';
		folderId = null;
		tag = null;
	}
	function toggleTag(t: string) {
		mode = 'catalogue';
		tag = tag === t ? null : t;
	}
</script>

<Dialog.Root bind:open>
	<Dialog.Content
		class="flex h-[80vh] w-full max-w-4xl flex-col gap-0 p-0 sm:max-w-4xl"
		data-testid="child-workflow-browser"
	>
		<Dialog.Header class="border-b border-border px-4 py-3 text-left">
			<Dialog.Title>Pick a child workflow</Dialog.Title>
		</Dialog.Header>

		<div class="flex min-h-0 flex-1">
			<!-- Folder / scope sidebar -->
			<aside class="w-56 shrink-0 overflow-y-auto border-r border-border bg-card/30 p-3">
				<div class="mb-2 flex items-center gap-2 text-sm font-medium text-foreground">
					<FolderTreeIcon class="size-4 text-muted-foreground" />
					Folders
				</div>
				<FolderTree
					{folders}
					selectedId={mode === 'catalogue' ? folderId : '__none__'}
					onSelect={selectFolder}
				/>

				{#if currentTemplateId}
					<div class="mt-4 mb-2 flex items-center gap-2 text-sm font-medium text-foreground">
						<Lock class="size-4 text-muted-foreground" />
						Scope
					</div>
					<button
						type="button"
						class="w-full rounded px-2 py-1 text-left text-sm hover:bg-accent {mode === 'private' ? 'bg-accent font-medium text-foreground' : 'text-muted-foreground'}"
						onclick={selectPrivate}
						data-testid="browser-scope-private"
					>
						Private to this workflow
					</button>
				{/if}

				{#if tags.length > 0}
					<div class="mt-4 mb-2 flex items-center gap-2 text-sm font-medium text-foreground">
						<Tag class="size-4 text-muted-foreground" />
						Tags
					</div>
					<div class="flex flex-wrap gap-1">
						{#each tags as t (t)}
							<button
								type="button"
								class="rounded border px-2 py-0.5 text-sm transition-colors {tag === t ? 'border-foreground bg-foreground text-background' : 'border-border text-muted-foreground hover:bg-accent'}"
								onclick={() => toggleTag(t)}
								data-testid={`browser-tag-${t}`}
							>
								{t}
							</button>
						{/each}
					</div>
				{/if}
			</aside>

			<!-- Search + results -->
			<div class="flex min-h-0 flex-1 flex-col">
				<div class="flex items-center gap-3 border-b border-border px-4 py-2">
					<div class="relative flex-1">
						<Search
							class="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground"
						/>
						<Input
							bind:value={search}
							placeholder="Search workflows…"
							class="h-9 pl-8"
							data-testid="browser-search"
						/>
					</div>
					{#if mode === 'catalogue'}
						<label class="flex items-center gap-1.5 text-sm text-muted-foreground">
							<input type="checkbox" bind:checked={publishedOnly} class="size-4" />
							Published only
						</label>
					{/if}
				</div>

				<div class="min-h-0 flex-1 overflow-y-auto p-4">
					{#if loading}
						<p class="py-8 text-center text-sm text-muted-foreground">Loading…</p>
					{:else if error}
						<p class="py-8 text-center text-sm text-destructive">{error}</p>
					{:else if filtered.length === 0}
						<p class="py-8 text-center text-sm text-muted-foreground">
							{mode === 'private'
								? 'No private sub-workflows yet for this workflow.'
								: 'No workflows match.'}
						</p>
					{:else}
						<div class="grid grid-cols-1 gap-2 sm:grid-cols-2">
							{#each filtered as t (t.id)}
								<button
									type="button"
									class="group flex flex-col gap-2 rounded-lg border border-border bg-card p-3 text-left transition-colors hover:bg-accent/50"
									onclick={() => pick(t)}
									data-testid={`browser-pick-${familyId(t)}`}
								>
									<div class="flex items-center justify-between gap-2">
										<span class="truncate text-sm font-medium text-foreground">{t.name}</span>
										<span
											class="rounded p-1 text-muted-foreground opacity-0 transition-opacity hover:bg-accent hover:text-foreground group-hover:opacity-100"
											role="button"
											tabindex="0"
											title="Open in new tab"
											onclick={(e) => openInTab(t, e)}
											onkeydown={(e) => {
												if (e.key === 'Enter' || e.key === ' ') openInTab(t, e as unknown as MouseEvent);
											}}
											data-testid={`browser-open-${familyId(t)}`}
										>
											<ExternalLink class="size-4" />
										</span>
									</div>
									<div class="flex items-center gap-1.5">
										<Badge
											variant="secondary"
											class={t.published ? 'bg-green-100 text-green-700' : 'bg-amber-100 text-amber-700'}
										>
											{t.published ? 'Published' : 'Draft'} v{t.version}
										</Badge>
										{#if t.visibility === 'private'}
											<Badge variant="secondary" class="bg-muted text-muted-foreground">
												<Lock class="mr-1 size-3" /> Private
											</Badge>
										{/if}
									</div>
									{#if t.description}
										<p class="truncate text-sm text-muted-foreground">{t.description}</p>
									{/if}
								</button>
							{/each}
						</div>
					{/if}
				</div>
			</div>
		</div>
	</Dialog.Content>
</Dialog.Root>
