<script lang="ts">
	import type { Snippet } from 'svelte';
	import { Label } from '$lib/components/ui/label';
	import { cn } from '$lib/utils.js';

	type Props = {
		/** The text label rendered above the input. */
		label?: string;
		/** The `for` attribute wired to the associated input's `id`. */
		for?: string;
		/** Optional helper text shown below the input. */
		description?: string;
		/** Error message; when present the text is rendered in destructive colour. */
		error?: string;
		/** Appends an asterisk to the label to signal a required field. */
		required?: boolean;
		/** Extra classes applied to the outer wrapper. */
		class?: string;
		children: Snippet;
	};

	let {
		label,
		for: htmlFor,
		description,
		error,
		required = false,
		class: className,
		children
	}: Props = $props();
</script>

<div data-slot="form-field" class={cn('flex flex-col gap-1.5', className)}>
	{#if label}
		<Label for={htmlFor} data-required={required || undefined}>
			{label}{#if required}<span class="text-destructive ml-0.5" aria-hidden="true">*</span>{/if}
		</Label>
	{/if}

	{@render children()}

	{#if error}
		<p data-slot="form-field-error" class="text-sm text-destructive">{error}</p>
	{:else if description}
		<p data-slot="form-field-description" class="text-sm text-muted-foreground">{description}</p>
	{/if}
</div>
