<script lang="ts">
	type Props = {
		src: string;
		filename: string;
	};

	let { src, filename }: Props = $props();
</script>

<div class="flex h-full flex-col">
	<div class="flex items-center justify-between border-b border-border bg-card px-4 py-2">
		<span class="text-xs font-medium text-muted-foreground">{filename}</span>
		<a
			href={src}
			target="_blank"
			rel="noopener noreferrer"
			class="text-xs text-muted-foreground underline transition-colors hover:text-foreground"
		>
			Open in new tab
		</a>
	</div>
	<div
		class="flex flex-1 items-center justify-center overflow-auto p-6"
		style="background-image: linear-gradient(45deg, var(--color-muted) 25%, transparent 25%),
			linear-gradient(-45deg, var(--color-muted) 25%, transparent 25%),
			linear-gradient(45deg, transparent 75%, var(--color-muted) 75%),
			linear-gradient(-45deg, transparent 75%, var(--color-muted) 75%);
			background-size: 16px 16px;
			background-position: 0 0, 0 8px, 8px -8px, -8px 0;"
	>
		<img
			{src}
			alt={filename}
			class="max-h-full max-w-full rounded object-contain shadow-md"
		/>
	</div>
</div>
