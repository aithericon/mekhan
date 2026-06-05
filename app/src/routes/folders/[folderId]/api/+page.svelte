<script lang="ts">
	import Copy from '@lucide/svelte/icons/copy';
	import ExternalLink from '@lucide/svelte/icons/external-link';
	import { Button } from '$lib/components/ui/button';
	import {
		Card,
		CardHeader,
		CardTitle,
		CardContent,
		CardDescription
	} from '$lib/components/ui/card';
	import FolderApiContract from '$lib/components/folders/FolderApiContract.svelte';
	import { getFolderContext } from '$lib/components/folders/folder-context';

	const ctx = getFolderContext();
	const folder = $derived(ctx.folder);

	// Bundle URL keys on the folder's own workspace, robust to switching.
	const bundleUrl = $derived(
		folder ? `/api/v1/workspaces/${folder.workspace_id}/folders/${folder.id}/openapi.json` : ''
	);

	async function copyBundleUrl() {
		const url = `${window.location.origin}${bundleUrl}`;
		try {
			await navigator.clipboard.writeText(url);
		} catch {
			prompt('Copy this URL', url);
		}
	}
</script>

<Card>
	<CardHeader>
		<div class="flex items-start justify-between gap-2">
			<div>
				<CardTitle>API</CardTitle>
				<CardDescription>Callable trigger contract synthesized from this folder's templates.</CardDescription>
			</div>
			{#if folder}
				<div class="flex gap-1">
					<Button variant="ghost" size="sm" title="Copy OpenAPI bundle URL" onclick={copyBundleUrl}>
						<Copy class="size-3.5" />
					</Button>
					<a
						href={bundleUrl}
						target="_blank"
						rel="noopener"
						class="inline-flex h-8 items-center justify-center rounded-md px-2 text-muted-foreground hover:bg-accent hover:text-foreground"
						title="Open OpenAPI bundle in new tab"
					>
						<ExternalLink class="size-3.5" />
					</a>
				</div>
			{/if}
		</div>
	</CardHeader>
	<CardContent>
		{#if folder}
			<FolderApiContract workspaceId={folder.workspace_id} folderId={folder.id} />
		{:else}
			<p class="text-sm text-muted-foreground">Loading…</p>
		{/if}
	</CardContent>
</Card>
