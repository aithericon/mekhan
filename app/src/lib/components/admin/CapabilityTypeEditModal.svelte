<script lang="ts">
	// Create-only Sheet for capability types (no PATCH route exists in the
	// API surface). Revoke + recreate is the intended edit flow for v1.
	// Each field row has: name, kind (FieldKind select), required checkbox,
	// and a conditional options editor shown only for kind === 'select'.
	import { Sheet, SheetContent, SheetTitle, SheetDescription, SheetClose } from '$lib/components/ui/sheet';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { FormField } from '$lib/components/ui/form-field';
	import * as Select from '$lib/components/ui/select';
	import { Checkbox } from '$lib/components/ui/checkbox';
	import X from '@lucide/svelte/icons/x';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import { createCapabilityType, type CapabilityField, type FieldKind } from '$lib/api/capability-types';

	// All FieldKind enum members as (value, label) pairs.
	const FIELD_KINDS: { value: FieldKind; label: string }[] = [
		{ value: 'text', label: 'Text' },
		{ value: 'textarea', label: 'Textarea' },
		{ value: 'number', label: 'Number' },
		{ value: 'bool', label: 'Boolean' },
		{ value: 'select', label: 'Select (enum)' },
		{ value: 'file', label: 'File' },
		{ value: 'signature', label: 'Signature' },
		{ value: 'timestamp', label: 'Timestamp' },
		{ value: 'json', label: 'JSON' }
	];

	type FieldRow = {
		name: string;
		kind: FieldKind;
		required: boolean;
		/** Comma/newline-separated options string — only used when kind === 'select'. */
		optionsRaw: string;
	};

	type Props = {
		open: boolean;
		onsaved: () => void;
	};

	let { open = $bindable(), onsaved }: Props = $props();

	let name = $state('');
	let fields = $state<FieldRow[]>([]);
	let loading = $state(false);
	let error = $state<string | null>(null);

	// Reset state whenever the modal opens.
	$effect(() => {
		if (!open) return;
		name = '';
		fields = [];
		error = null;
		loading = false;
	});

	function addField() {
		fields = [...fields, { name: '', kind: 'text', required: false, optionsRaw: '' }];
	}

	function removeField(index: number) {
		fields = fields.filter((_, i) => i !== index);
	}

	function updateField<K extends keyof FieldRow>(index: number, key: K, value: FieldRow[K]) {
		fields = fields.map((f, i) => (i === index ? { ...f, [key]: value } : f));
	}

	// Validate and build the CapabilityField array to POST.
	const fieldErrors = $derived.by(() => {
		const errs: string[] = [];
		const seen = new Set<string>();
		for (const [i, f] of fields.entries()) {
			const n = f.name.trim();
			if (!n) {
				errs.push(`Field ${i + 1}: name is required.`);
				continue;
			}
			if (!/^[a-z][a-z0-9_]*$/.test(n)) {
				errs.push(`Field ${i + 1}: "${n}" must be snake_case (lowercase letter first).`);
			}
			if (seen.has(n)) errs.push(`Field ${i + 1}: duplicate name "${n}".`);
			seen.add(n);
			if (f.kind === 'select') {
				const opts = parseOptions(f.optionsRaw);
				if (opts.length === 0) {
					errs.push(`Field ${i + 1}: select kind requires at least one option.`);
				}
			}
		}
		return errs;
	});

	function parseOptions(raw: string): string[] {
		return raw
			.split(/[,\n]/)
			.map((s) => s.trim())
			.filter(Boolean);
	}

	function buildFields(): CapabilityField[] {
		return fields.map((f) => ({
			name: f.name.trim(),
			kind: f.kind,
			required: f.required,
			options: f.kind === 'select' ? parseOptions(f.optionsRaw) : undefined
		}));
	}

	async function submit() {
		const trimmed = name.trim();
		if (!trimmed) {
			error = 'Capability type name is required.';
			return;
		}
		if (fieldErrors.length > 0) {
			error = fieldErrors[0];
			return;
		}
		loading = true;
		error = null;
		try {
			await createCapabilityType({ name: trimmed, fields: buildFields() });
			onsaved();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Save failed';
		} finally {
			loading = false;
		}
	}
</script>

<Sheet.Root bind:open>
	<SheetContent class="w-[540px] sm:max-w-[540px]">
		<div class="flex items-center justify-between border-b border-border px-5 py-4">
			<div>
				<SheetTitle class="text-lg font-semibold">New capability type</SheetTitle>
				<SheetDescription class="text-sm text-muted-foreground">
					Defines a typed schema that runners advertise. Steps use
					<code>requirements</code> to match.
				</SheetDescription>
			</div>
			<SheetClose>
				<X class="size-4" />
			</SheetClose>
		</div>

		<div class="flex flex-1 flex-col overflow-y-auto px-5 py-4">
			{#if error}
				<div
					class="mb-4 rounded-md border border-destructive/30 bg-destructive/10 px-3 py-2 text-sm text-destructive"
				>
					{error}
				</div>
			{/if}

			<div class="space-y-5">
				<FormField label="Name" required>
					<Input
						type="text"
						value={name}
						placeholder="gpu_spec"
						oninput={(e) => (name = (e.currentTarget as HTMLInputElement).value)}
						class="font-mono text-sm"
						data-testid="cap-type-name"
					/>
				</FormField>

				<div class="space-y-2">
					<div class="flex items-center justify-between">
						<span class="text-sm font-medium text-foreground">Fields</span>
						<Button variant="outline" size="sm" onclick={addField} class="gap-1.5">
							<Plus class="size-3.5" />
							Add field
						</Button>
					</div>

					{#if fields.length === 0}
						<p class="py-4 text-center text-sm text-muted-foreground italic">
							No fields yet. A capability type can have zero or more typed fields.
						</p>
					{/if}

					<div class="space-y-3">
						{#each fields as field, i (i)}
							<div
								class="rounded-md border border-border/60 bg-muted/20 p-3 space-y-3"
								data-testid="cap-field-row-{i}"
							>
								<div class="flex items-start gap-2">
									<div class="flex-1 space-y-2">
										<div class="flex items-start gap-2">
											<div class="flex-1">
												<FormField label="Field name">
													<Input
														type="text"
														value={field.name}
														placeholder="vram_gb"
														oninput={(e) =>
															updateField(
																i,
																'name',
																(e.currentTarget as HTMLInputElement).value
															)}
														class="font-mono text-sm"
														data-testid="cap-field-name-{i}"
													/>
												</FormField>
											</div>
											<div class="w-44">
												<FormField label="Kind">
													<Select.Root
														type="single"
														value={field.kind}
														onValueChange={(v) =>
															updateField(i, 'kind', (v ?? 'text') as FieldKind)}
													>
														<Select.Trigger
															class="w-full text-sm"
															data-testid="cap-field-kind-{i}"
														>
															{FIELD_KINDS.find((k) => k.value === field.kind)?.label ??
																field.kind}
														</Select.Trigger>
														<Select.Content>
															{#each FIELD_KINDS as k (k.value)}
																<Select.Item value={k.value} label={k.label} />
															{/each}
														</Select.Content>
													</Select.Root>
												</FormField>
											</div>
										</div>

										<div class="flex items-center gap-2">
											<Checkbox
												id="field-required-{i}"
												checked={field.required}
												onCheckedChange={(v) => updateField(i, 'required', !!v)}
												data-testid="cap-field-required-{i}"
											/>
											<label
												for="field-required-{i}"
												class="cursor-pointer text-sm text-muted-foreground select-none"
											>
												Required
											</label>
										</div>

										{#if field.kind === 'select'}
											<FormField
												label="Options"
												description="One option per line, or comma-separated."
											>
												<textarea
													value={field.optionsRaw}
													placeholder={"small\nmedium\nlarge"}
													oninput={(e) =>
														updateField(
															i,
															'optionsRaw',
															(e.currentTarget as HTMLTextAreaElement).value
														)}
													rows={3}
													class="w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-sm placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50 disabled:cursor-not-allowed disabled:opacity-50"
													data-testid="cap-field-options-{i}"
												></textarea>
											</FormField>
										{/if}
									</div>

									<Button
										variant="ghost"
										size="sm"
										onclick={() => removeField(i)}
										aria-label="Remove field"
										class="mt-5 text-muted-foreground hover:text-destructive"
									>
										<Trash2 class="size-3.5" />
									</Button>
								</div>
							</div>
						{/each}
					</div>
				</div>
			</div>
		</div>

		<div
			class="flex items-center justify-end gap-2 border-t border-border bg-muted/30 px-5 py-3"
		>
			<Button variant="ghost" size="sm" onclick={() => (open = false)}>Cancel</Button>
			<Button size="sm" onclick={submit} disabled={loading}>
				{loading ? 'Creating…' : 'Create capability type'}
			</Button>
		</div>
	</SheetContent>
</Sheet.Root>
