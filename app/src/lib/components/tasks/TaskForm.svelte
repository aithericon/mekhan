<script lang="ts">
	import type { TaskStep, TaskField } from '@aithericon/hpi-ui/types';
	import { BlockRenderer } from '@aithericon/hpi-ui';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { Textarea } from '$lib/components/ui/textarea';
	import { Label } from '$lib/components/ui/label';

	interface Props {
		steps: TaskStep[];
		onsubmit: (data: Record<string, unknown>) => void;
		oncancel?: (reason?: string) => void;
		submitting?: boolean;
	}

	let { steps, onsubmit, oncancel, submitting = false }: Props = $props();

	let formData: Record<string, unknown> = $state({});
	let validationErrors: Record<string, string> = $state({});

	// Collect all input fields across steps
	const inputFields = $derived(
		steps.flatMap((step) =>
			step.blocks
				.filter((b): b is Extract<typeof b, { type: 'input' }> => b.type === 'input')
				.map((b) => b.field)
		)
	);

	function validate(): boolean {
		const errors: Record<string, string> = {};
		for (const field of inputFields) {
			if (field.required) {
				const val = formData[field.name];
				if (val === undefined || val === null || val === '') {
					errors[field.name] = `${field.label} is required`;
				}
			}
		}
		validationErrors = errors;
		return Object.keys(errors).length === 0;
	}

	function handleSubmit() {
		if (validate()) {
			onsubmit(formData);
		}
	}

	function handleCancel() {
		oncancel?.();
	}

	function updateField(name: string, value: unknown) {
		formData = { ...formData, [name]: value };
		// Clear validation error on change
		if (validationErrors[name]) {
			const { [name]: _, ...rest } = validationErrors;
			validationErrors = rest;
		}
	}
</script>

<form
	class="flex flex-col gap-6"
	onsubmit={(e) => {
		e.preventDefault();
		handleSubmit();
	}}
>
	{#each steps as step (step.id)}
		<div class="space-y-4">
			{#if steps.length > 1}
				<h3 class="text-sm font-medium text-foreground">{step.title}</h3>
			{/if}

			{#if step.description_mdsvex}
				<p class="text-xs text-muted-foreground">{step.description_mdsvex}</p>
			{/if}

			{#each step.blocks as block}
				{#if block.type === 'input'}
					{@const field = block.field}
					<div class="space-y-1.5">
						<Label for={field.name}>
							{field.label}
							{#if field.required}
								<span class="text-destructive">*</span>
							{/if}
						</Label>

						{#if field.description_mdsvex}
							<p class="text-xs text-muted-foreground">{field.description_mdsvex}</p>
						{/if}

						{#if field.kind === 'text'}
							<Input
								id={field.name}
								type="text"
								placeholder={field.placeholder ?? ''}
								value={formData[field.name] as string ?? ''}
								oninput={(e: Event) => updateField(field.name, (e.target as HTMLInputElement).value)}
							/>
						{:else if field.kind === 'textarea'}
							<Textarea
								id={field.name}
								placeholder={field.placeholder ?? ''}
								value={formData[field.name] as string ?? ''}
								oninput={(e: Event) => updateField(field.name, (e.target as HTMLTextAreaElement).value)}
								rows={4}
							/>
						{:else if field.kind === 'number'}
							<Input
								id={field.name}
								type="number"
								placeholder={field.placeholder ?? ''}
								min={field.min}
								max={field.max}
								step={field.step}
								value={formData[field.name] as string ?? ''}
								oninput={(e: Event) => {
									const v = (e.target as HTMLInputElement).value;
									updateField(field.name, v === '' ? undefined : Number(v));
								}}
							/>
						{:else if field.kind === 'select'}
							<select
								id={field.name}
								class="flex h-9 w-full rounded-md border border-input bg-transparent px-3 py-1 text-sm shadow-sm transition-colors focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
								value={formData[field.name] as string ?? ''}
								onchange={(e: Event) => updateField(field.name, (e.target as HTMLSelectElement).value)}
							>
								<option value="" disabled>{field.placeholder ?? 'Select...'}</option>
								{#if field.options}
									{#each field.options as opt}
										<option value={opt}>{opt}</option>
									{/each}
								{/if}
							</select>
						{:else if field.kind === 'checkbox'}
							<label class="flex items-center gap-2 text-sm">
								<input
									type="checkbox"
									id={field.name}
									checked={!!formData[field.name]}
									onchange={(e: Event) => updateField(field.name, (e.target as HTMLInputElement).checked)}
									class="size-4 rounded border-input"
								/>
								<span class="text-muted-foreground">{field.placeholder ?? ''}</span>
							</label>
						{:else if field.kind === 'radio' && field.options}
							<div class="flex flex-col gap-2">
								{#each field.options as opt}
									<label class="flex items-center gap-2 text-sm">
										<input
											type="radio"
											name={field.name}
											value={opt}
											checked={formData[field.name] === opt}
											onchange={() => updateField(field.name, opt)}
											class="size-4 border-input"
										/>
										{opt}
									</label>
								{/each}
							</div>
						{:else if field.kind === 'date'}
							<Input
								id={field.name}
								type={field.include_time ? 'datetime-local' : 'date'}
								value={formData[field.name] as string ?? ''}
								oninput={(e: Event) => updateField(field.name, (e.target as HTMLInputElement).value)}
							/>
						{:else if field.kind === 'range'}
							<div class="flex items-center gap-3">
								<input
									type="range"
									id={field.name}
									min={field.min ?? 0}
									max={field.max ?? 100}
									step={field.step ?? 1}
									value={formData[field.name] as number ?? field.min ?? 0}
									oninput={(e: Event) => updateField(field.name, Number((e.target as HTMLInputElement).value))}
									class="flex-1"
								/>
								<span class="text-sm tabular-nums text-muted-foreground w-12 text-right">
									{formData[field.name] ?? field.min ?? 0}
								</span>
							</div>
						{:else if field.kind === 'rating'}
							<div class="flex gap-1">
								{#each Array.from({ length: field.max_rating ?? 5 }) as _, i}
									<button
										type="button"
										class="text-lg transition-colors {(formData[field.name] as number ?? 0) > i ? 'text-amber-400' : 'text-muted-foreground/30'}"
										onclick={() => updateField(field.name, i + 1)}
									>
										&#9733;
									</button>
								{/each}
							</div>
						{:else if field.kind === 'file' || field.kind === 'signature'}
							<div class="rounded-md border border-dashed border-border px-3 py-4 text-center text-xs text-muted-foreground">
								{field.kind === 'file' ? 'File upload' : 'Signature'} not available in Mekhan
							</div>
						{/if}

						{#if validationErrors[field.name]}
							<p class="text-xs text-destructive">{validationErrors[field.name]}</p>
						{/if}
					</div>
				{:else}
					<BlockRenderer {block} />
				{/if}
			{/each}
		</div>
	{/each}

	<!-- Action bar -->
	<div class="flex items-center gap-2 border-t border-border pt-4">
		<Button type="submit" disabled={submitting}>
			{submitting ? 'Submitting...' : 'Complete Task'}
		</Button>
		{#if oncancel}
			<Button type="button" variant="outline" onclick={handleCancel} disabled={submitting}>
				Cancel Task
			</Button>
		{/if}
	</div>
</form>
