<script lang="ts">
	import Save from '@lucide/svelte/icons/save';
	import Upload from '@lucide/svelte/icons/upload';
	import Eye from '@lucide/svelte/icons/eye';
	import Undo from '@lucide/svelte/icons/undo';

	type Props = {
		templateName: string;
		published: boolean;
		saving: boolean;
		onsave: () => void;
		onpublish: () => void;
		onpreview: () => void;
	};

	let { templateName, published, saving, onsave, onpublish, onpreview }: Props = $props();
</script>

<div class="flex h-10 items-center justify-between border-b border-border bg-card px-3">
	<div class="flex items-center gap-3">
		<span class="text-sm font-medium text-foreground">{templateName}</span>
		{#if published}
			<span class="rounded-full bg-green-100 px-2 py-0.5 text-[10px] font-medium text-green-700">
				Published
			</span>
		{:else}
			<span class="rounded-full bg-amber-100 px-2 py-0.5 text-[10px] font-medium text-amber-700">
				Draft
			</span>
		{/if}
	</div>

	<div class="flex items-center gap-1.5">
		<button
			type="button"
			class="flex items-center gap-1.5 rounded-md px-2.5 py-1 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-foreground disabled:opacity-50"
			onclick={onpreview}
		>
			<Eye class="size-3.5" />
			Preview AIR
		</button>

		<button
			type="button"
			class="flex items-center gap-1.5 rounded-md px-2.5 py-1 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-foreground disabled:opacity-50"
			disabled={saving || published}
			onclick={onsave}
		>
			<Save class="size-3.5" />
			{saving ? 'Saving...' : 'Save'}
		</button>

		<button
			type="button"
			class="flex items-center gap-1.5 rounded-md bg-primary px-2.5 py-1 text-xs text-primary-foreground transition-colors hover:bg-primary/90 disabled:opacity-50"
			disabled={published}
			onclick={onpublish}
		>
			<Upload class="size-3.5" />
			Publish
		</button>
	</div>
</div>
