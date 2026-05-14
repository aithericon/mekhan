<script lang="ts">
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import ImageIcon from '@lucide/svelte/icons/image';
	import X from '@lucide/svelte/icons/x';
	import Grid2x2 from '@lucide/svelte/icons/grid-2x2';
	import GalleryHorizontal from '@lucide/svelte/icons/gallery-horizontal';
	import Square from '@lucide/svelte/icons/square';

	type DisplayMode = 'single' | 'grid' | 'gallery';

	type Props = {
		filenames: string[];
		display: DisplayMode;
		binding?: YjsGraphBinding;
		nodeId?: string;
		readonly?: boolean;
		onchange: (filenames: string[], display: DisplayMode) => void;
		onremove: () => void;
	};

	let { filenames, display, binding, nodeId, readonly = false, onchange, onremove }: Props = $props();

	const IMAGE_EXTENSIONS = ['.png', '.jpg', '.jpeg', '.gif', '.webp', '.svg'];

	function isImageFile(name: string): boolean {
		return IMAGE_EXTENSIONS.some((ext) => name.toLowerCase().endsWith(ext));
	}

	const imageFiles = $derived.by(() => {
		if (!binding || !nodeId) return [];
		const files = binding.getNodeFiles(nodeId);
		return [...files.entries()]
			.filter(([name]) => isImageFile(name))
			.map(([name, ytext]) => ({ name, key: ytext.toString() }));
	});

	const availableFiles = $derived(
		imageFiles.filter((f) => !filenames.includes(f.name))
	);

	function getImageSrc(filename: string): string | null {
		const file = imageFiles.find((f) => f.name === filename);
		return file?.key ? `/api/files/${file.key}` : null;
	}

	function addFile(name: string) {
		onchange([...filenames, name], display);
	}

	function removeFile(name: string) {
		onchange(filenames.filter((f) => f !== name), display);
	}

	function setDisplay(mode: DisplayMode) {
		onchange(filenames, mode);
	}

	const displayModes: { mode: DisplayMode; label: string }[] = [
		{ mode: 'single', label: 'Single' },
		{ mode: 'grid', label: 'Grid' },
		{ mode: 'gallery', label: 'Gallery' }
	];
</script>

<!-- ui-allow: block-type accent — no theme token for image/emerald identity -->
<div class="rounded-md border border-border/50 border-l-2 border-l-emerald-400 bg-background p-3">
	<!-- Header -->
	<div class="mb-2 flex items-center justify-between">
		<!-- ui-allow: block-type badge color — no theme token for image/emerald identity -->
		<span class="rounded bg-emerald-100 px-2 py-0.5 text-xs font-medium text-emerald-700 dark:bg-emerald-900/30 dark:text-emerald-300">
			Image
		</span>
		<div class="flex items-center gap-1">
			{#if !readonly}
				<!-- Display mode toggle -->
				{#each displayModes as { mode, label } (mode)}
					<button
						type="button"
						class="rounded p-1 transition-colors {display === mode
							? 'bg-accent text-foreground'
							: 'text-muted-foreground hover:text-foreground'}"
						onclick={() => setDisplay(mode)}
						title={label}
					>
						{#if mode === 'single'}
							<Square class="size-3.5" />
						{:else if mode === 'grid'}
							<Grid2x2 class="size-3.5" />
						{:else}
							<GalleryHorizontal class="size-3.5" />
						{/if}
					</button>
				{/each}
				<div class="mx-1 h-4 w-px bg-border"></div>
				<button
					type="button"
					class="rounded p-1 text-muted-foreground transition-colors hover:text-destructive"
					onclick={onremove}
				>
					<Trash2 class="size-4" />
				</button>
			{/if}
		</div>
	</div>

	{#if imageFiles.length === 0}
		<div class="flex items-center gap-2 rounded-md border border-dashed border-border p-4 text-sm text-muted-foreground">
			<ImageIcon class="size-4 shrink-0" />
			<span>No images uploaded yet. Use the upload button in the file tree.</span>
		</div>
	{:else}
		<div class="space-y-3">
			<!-- Selected images preview -->
			{#if filenames.length > 0}
				{#if display === 'single'}
					<!-- Single: show first image large -->
					{@const src = getImageSrc(filenames[0])}
					{#if src}
						<div class="overflow-hidden rounded-md border border-border bg-muted/30">
							<img src={src} alt={filenames[0]} class="mx-auto max-h-56 object-contain" />
						</div>
					{/if}
					{#if filenames.length > 1}
						<p class="text-xs text-muted-foreground">+{filenames.length - 1} more (switch to grid/gallery to see all)</p>
					{/if}
				{:else if display === 'grid'}
					<!-- Grid: 2-column grid of thumbnails -->
					<div class="grid grid-cols-2 gap-2">
						{#each filenames as name (name)}
							{@const src = getImageSrc(name)}
							<div class="group relative overflow-hidden rounded-md border border-border bg-muted/30">
								{#if src}
									<img src={src} alt={name} class="aspect-square w-full object-cover" />
								{:else}
									<div class="flex aspect-square items-center justify-center">
										<ImageIcon class="size-6 text-muted-foreground" />
									</div>
								{/if}
								{#if !readonly}
									<button
										type="button"
										class="absolute right-1 top-1 rounded-full bg-background/80 p-0.5 text-muted-foreground opacity-0 backdrop-blur-sm transition-opacity group-hover:opacity-100 hover:text-destructive"
										onclick={() => removeFile(name)}
									>
										<X class="size-3" />
									</button>
								{/if}
								<div class="absolute inset-x-0 bottom-0 bg-gradient-to-t from-black/50 to-transparent px-1.5 py-1">
									<span class="text-[10px] font-mono text-white drop-shadow">{name}</span>
								</div>
							</div>
						{/each}
					</div>
				{:else}
					<!-- Gallery: horizontal scroll row -->
					<div class="flex gap-2 overflow-x-auto pb-1">
						{#each filenames as name (name)}
							{@const src = getImageSrc(name)}
							<div class="group relative shrink-0 overflow-hidden rounded-md border border-border bg-muted/30">
								{#if src}
									<img src={src} alt={name} class="h-32 w-auto object-contain" />
								{:else}
									<div class="flex h-32 w-32 items-center justify-center">
										<ImageIcon class="size-6 text-muted-foreground" />
									</div>
								{/if}
								{#if !readonly}
									<button
										type="button"
										class="absolute right-1 top-1 rounded-full bg-background/80 p-0.5 text-muted-foreground opacity-0 backdrop-blur-sm transition-opacity group-hover:opacity-100 hover:text-destructive"
										onclick={() => removeFile(name)}
									>
										<X class="size-3" />
									</button>
								{/if}
								<div class="absolute inset-x-0 bottom-0 bg-gradient-to-t from-black/50 to-transparent px-1.5 py-1">
									<span class="text-[10px] font-mono text-white drop-shadow">{name}</span>
								</div>
							</div>
						{/each}
					</div>
				{/if}
			{/if}

			<!-- Add image picker -->
			{#if !readonly && availableFiles.length > 0}
				<div class="flex flex-wrap gap-1.5">
					{#each availableFiles as img (img.name)}
						<button
							type="button"
							class="flex items-center gap-1 rounded-md border border-dashed border-border px-2 py-1 text-xs text-muted-foreground transition-colors hover:border-foreground/30 hover:bg-accent hover:text-foreground"
							onclick={() => addFile(img.name)}
						>
							<ImageIcon class="size-3 shrink-0" />
							<span class="max-w-[120px] truncate font-mono">{img.name}</span>
						</button>
					{/each}
				</div>
			{/if}
		</div>
	{/if}
</div>
