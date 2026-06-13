<!--
  PageEditorScreen — full-screen host for a free page at `/pages/[id]`.

  Loads the page row for its title + host effective role, derives `editable` via
  roleAtLeast, mounts <PageEditor>, and links back to the page's home folder.

  Uses PageShell `width="bleed"` (the editor owns its own scroll, no width cap),
  with a self-composed pinned header inside the bleed body (a true bleed shell
  provides no band slot).
-->
<script lang="ts">
	import ChevronLeft from '@lucide/svelte/icons/chevron-left';
	import FileText from '@lucide/svelte/icons/file-text';
	import PageShell from '$lib/components/shell/PageShell.svelte';
	import PageEditor from './PageEditor.svelte';
	import { getPage, type Page } from '$lib/api/client';
	import { roleAtLeast } from '$lib/api/iam';

	let { pageId }: { pageId: string } = $props();

	let page = $state<Page | null>(null);
	let loadError = $state<string | null>(null);

	$effect(() => {
		// Re-fetch whenever the pageId changes.
		const id = pageId;
		page = null;
		loadError = null;
		getPage(id)
			.then((p) => {
				if (id === pageId) page = p;
			})
			.catch((e) => {
				if (id === pageId) loadError = e instanceof Error ? e.message : String(e);
			});
	});

	const editable = $derived(roleAtLeast(page?.my_effective_role, 'editor'));
	const backHref = $derived(
		page?.folder_id ? `/folders?folder=${page.folder_id}` : '/folders'
	);
	const title = $derived(page?.title?.trim() || 'Untitled page');
</script>

<svelte:head>
	<title>{title} | Mekhan</title>
</svelte:head>

<PageShell width="bleed" testid="page-editor-screen">
	<div class="flex h-full min-h-0 flex-col">
		<!-- Pinned header band. -->
		<div class="bg-card shrink-0 border-b border-border px-6 py-3">
			<div class="mx-auto w-full max-w-4xl">
				<a
					href={backHref}
					class="text-muted-foreground hover:text-foreground mb-1 inline-flex items-center gap-1 text-sm transition-colors"
				>
					<ChevronLeft class="size-4" />
					Back to folder
				</a>
				<div class="flex items-center gap-2">
					<FileText class="text-muted-foreground size-5" />
					<h1 class="text-lg font-semibold" data-testid="page-title">{title}</h1>
				</div>
			</div>
		</div>

		<!-- Editor body. -->
		<div class="min-h-0 flex-1 overflow-hidden">
			<div class="mx-auto h-full w-full max-w-4xl px-6 py-4">
				{#if loadError}
					<p class="text-destructive text-sm">Failed to load page: {loadError}</p>
				{:else if page}
					<PageEditor pageId={page.id} {editable} placeholder="Start writing this page…" />
				{:else}
					<p class="text-muted-foreground text-sm">Loading…</p>
				{/if}
			</div>
		</div>
	</div>
</PageShell>
