<script lang="ts">
	// SPDX-License-Identifier: Apache-2.0
	import { displaySize } from '../utils';
	import type { DownloadItem } from '../types';
	import FileText from '@lucide/svelte/icons/file-text';
	import ImageIcon from '@lucide/svelte/icons/image';
	import Table from '@lucide/svelte/icons/table';
	import Archive from '@lucide/svelte/icons/archive';
	import FileCode from '@lucide/svelte/icons/file-code';
	import Video from '@lucide/svelte/icons/video';
	import Music from '@lucide/svelte/icons/music';
	import File from '@lucide/svelte/icons/file';

	let { downloads }: { downloads: DownloadItem[] } = $props();

	const iconMap: [RegExp, typeof File][] = [
		[/^application\/pdf/, FileText],
		[/^image\//, ImageIcon],
		[/text\/csv|spreadsheet/, Table],
		[/zip|gzip|tar|compressed|archive/, Archive],
		[/^application\/json|^text\//, FileCode],
		[/^video\//, Video],
		[/^audio\//, Music]
	];

	function getIcon(mimeType?: string): typeof File {
		if (!mimeType) return File;
		for (const [pattern, icon] of iconMap) {
			if (pattern.test(mimeType)) return icon;
		}
		return File;
	}

	function mimeLabel(mimeType?: string): string {
		if (!mimeType) return '';
		const parts = mimeType.split('/');
		return parts[1]?.replace(/^vnd\.|^x-/, '').toUpperCase() ?? mimeType.toUpperCase();
	}
</script>

<div class="space-y-3">
	{#each downloads as item (item.url)}
		{@const Icon = getIcon(item.mime_type)}
		<div class="flex items-center gap-4 rounded-xl border border-border bg-card/70 p-4 shadow-sm">
			<div class="flex size-10 shrink-0 items-center justify-center rounded-lg bg-muted/40">
				{#if item.thumbnail_url}
					<img src={item.thumbnail_url} alt="" class="size-10 rounded-lg object-cover" />
				{:else}
					<Icon class="size-5 text-muted-foreground" />
				{/if}
			</div>

			<div class="min-w-0 flex-1">
				<a
					href={item.url}
					target="_blank"
					rel="noopener noreferrer"
					download={item.filename}
					class="text-sm font-medium text-foreground hover:text-primary hover:underline"
				>
					{item.filename}
				</a>
				<div class="mt-0.5 flex flex-wrap items-center gap-2 text-sm text-muted-foreground">
					{#if item.mime_type}
						<span class="rounded-md bg-muted/50 px-1.5 py-0.5 text-sm font-medium uppercase">
							{mimeLabel(item.mime_type)}
						</span>
					{/if}
					{#if item.size != null}
						<span>{displaySize(item.size)}</span>
					{/if}
				</div>
				{#if item.description}
					<p class="mt-1 text-sm text-muted-foreground">{item.description}</p>
				{/if}
			</div>
		</div>
	{/each}
</div>
