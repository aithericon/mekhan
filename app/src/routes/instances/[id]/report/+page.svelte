<!--
  Instance Report — a collaborative rich-text page attached to this run.

  Always available (even before the run started). Uses the upsert singleton
  endpoint via ensureAttachedPage('instance', …) so the page is created lazily
  on first visit. editable is derived from the instance's effective role; the
  PageEditor itself is ACL-dumb.
-->
<script lang="ts">
	import { ensureAttachedPage, type Page } from '$lib/api/client';
	import { useInstanceContext } from '$lib/components/instances/instance-context';
	import PageEditor from '$lib/components/pages/PageEditor.svelte';
	import { roleAtLeast } from '$lib/api/iam';
	import FileText from '@lucide/svelte/icons/file-text';

	const ctx = useInstanceContext();

	let pageRecord = $state<Page | null>(null);
	let loading = $state(false);
	let error = $state<string | null>(null);

	// Resolve (get-or-create) the singleton page attached to this instance once
	// the instance is loaded. The instance id is stable for the page's lifetime,
	// so we key the load on it and only fetch once per instance.
	let resolvedFor = $state<string | null>(null);
	$effect(() => {
		const id = ctx.instance?.id;
		if (!id || resolvedFor === id) return;
		resolvedFor = id;
		pageRecord = null;
		loading = true;
		error = null;
		ensureAttachedPage('instance', id)
			.then((p) => {
				pageRecord = p;
			})
			.catch((e) => {
				error = e instanceof Error ? e.message : 'Failed to load report';
			})
			.finally(() => {
				loading = false;
			});
	});

	const editable = $derived(roleAtLeast(ctx.instance?.my_effective_role, 'editor'));
</script>

<div class="absolute inset-0 overflow-y-auto">
	{#if loading && !pageRecord}
		<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">
			Loading report…
		</div>
	{:else if error}
		<div
			class="mx-6 mt-6 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800"
		>
			{error}
		</div>
	{:else if pageRecord}
		{@const p = pageRecord}
		<div class="mx-auto h-full w-full max-w-4xl px-6 py-6">
			{#key p.id}
				<PageEditor
					pageId={p.id}
					{editable}
					placeholder={editable
						? 'Write a report for this run…'
						: 'No report has been written for this run.'}
				/>
			{/key}
		</div>
	{:else}
		<div
			class="flex h-full flex-col items-center justify-center gap-2 py-16 text-sm text-muted-foreground"
		>
			<FileText class="size-8 text-muted-foreground/40" />
			<p>No report for this run yet.</p>
		</div>
	{/if}
</div>
