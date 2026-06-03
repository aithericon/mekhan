<script lang="ts">
	// Placement-requirements editor for an AutomatedStep node.
	// Lets the author express typed ClassAd-style constraints over runner capabilities
	// so the engine's `satisfies(requirements, caps)` matcher selects an appropriate
	// pool unit. Empty constraints → matches any runner (default).
	//
	// Follows the EXACT same patch+onchange idiom as RetryPolicySection — no bind:,
	// every mutation rebuilds and calls onchange({ ...data, requirements: {...} }).
	import type { AutomatedStepNodeData } from '$lib/types/editor';
	import type { components } from '$lib/api/schema';
	import { onMount } from 'svelte';
	import * as Select from '$lib/components/ui/select';
	import { Input } from '$lib/components/ui/input';
	import { Button } from '$lib/components/ui/button';
	import { FormField } from '$lib/components/ui/form-field';
	import { listCapabilityTypes, type CapabilityTypeSummary } from '$lib/api/capability-types';

	type Constraint = components['schemas']['Constraint'];
	type ConstraintOp = components['schemas']['ConstraintOp'];

	// Ops that take a value input
	const OPS_WITH_VALUE: ConstraintOp[] = ['eq', 'neq', 'gt', 'gte', 'lt', 'lte', 'in'];
	// Ops that take NO value input
	const OPS_WITHOUT_VALUE: ConstraintOp[] = ['exists'];

	const OP_LABELS: Record<ConstraintOp, string> = {
		eq: 'eq (=)',
		neq: 'neq (≠)',
		gt: 'gt (>)',
		gte: 'gte (≥)',
		lt: 'lt (<)',
		lte: 'lte (≤)',
		in: 'in (list)',
		exists: 'exists'
	};

	const ALL_OPS: ConstraintOp[] = [...OPS_WITH_VALUE, ...OPS_WITHOUT_VALUE];

	type Props = {
		data: AutomatedStepNodeData;
		readonly?: boolean;
		onchange: (data: AutomatedStepNodeData) => void;
	};

	let { data, readonly = false, onchange }: Props = $props();

	// ── Capability types (loaded once on mount) ────────────────────────────────
	let capabilityTypes = $state<CapabilityTypeSummary[]>([]);
	let capTypesLoaded = $state(false);

	onMount(() => {
		listCapabilityTypes({ perPage: 200 })
			.then((p) => {
				capabilityTypes = p.items;
				capTypesLoaded = true;
			})
			.catch(() => {
				capTypesLoaded = true; // still set so we don't show a spinner forever
			});
	});

	// ── Local constraint list, derived from data ───────────────────────────────
	const constraints = $derived<Constraint[]>(data.requirements?.constraints ?? []);

	// ── Helpers ────────────────────────────────────────────────────────────────

	/** Fields available for a given capability name. */
	function fieldsForCap(capName: string): string[] {
		const cap = capabilityTypes.find((c) => c.name === capName);
		return cap ? cap.fields.map((f) => f.name) : [];
	}

	/** FieldKind for a given capability + field name. */
	function kindForField(capName: string, fieldName: string): string {
		const cap = capabilityTypes.find((c) => c.name === capName);
		return cap?.fields.find((f) => f.name === fieldName)?.kind ?? 'text';
	}

	/** Options for a Select-kind field. */
	function optionsForField(capName: string, fieldName: string): string[] {
		const cap = capabilityTypes.find((c) => c.name === capName);
		return cap?.fields.find((f) => f.name === fieldName)?.options ?? [];
	}

	// ── Mutation helpers ───────────────────────────────────────────────────────

	function emit(next: Constraint[]) {
		if (next.length === 0) {
			// Omit the field entirely when the list is empty (cleaner wire shape).
			// eslint-disable-next-line @typescript-eslint/no-unused-vars
			const { requirements: _r, ...rest } = data as AutomatedStepNodeData & {
				requirements?: unknown;
			};
			onchange(rest as AutomatedStepNodeData);
		} else {
			onchange({ ...data, requirements: { constraints: next } });
		}
	}

	function addConstraint() {
		const first = capabilityTypes[0];
		const firstField = first?.fields[0]?.name ?? '';
		emit([
			...constraints,
			{ capability: first?.name ?? '', field: firstField, op: 'eq', value: '' }
		]);
	}

	function removeConstraint(idx: number) {
		emit(constraints.filter((_, i) => i !== idx));
	}

	function patchConstraint(idx: number, patch: Partial<Constraint>) {
		const next = constraints.map((c, i) => (i === idx ? { ...c, ...patch } : c));
		emit(next);
	}

	/** When capability changes, reset field to first available + keep op. */
	function setCapability(idx: number, cap: string) {
		const fields = fieldsForCap(cap);
		patchConstraint(idx, { capability: cap, field: fields[0] ?? '', value: undefined });
	}

	/** When field changes, reset value to be safe. */
	function setField(idx: number, field: string) {
		patchConstraint(idx, { field, value: undefined });
	}

	function setOp(idx: number, op: ConstraintOp) {
		// 'exists' has no value
		if (op === 'exists') {
			patchConstraint(idx, { op, value: undefined });
		} else {
			patchConstraint(idx, { op });
		}
	}

	/** Parse a typed value from a text input based on field kind. */
	function parseValue(raw: string, kind: string): unknown {
		if (kind === 'number') {
			const n = parseFloat(raw);
			return isNaN(n) ? raw : n;
		}
		if (kind === 'bool') return raw === 'true';
		return raw;
	}

	/** Stringify a value for display in an input. */
	function displayValue(value: unknown): string {
		if (value === undefined || value === null) return '';
		return String(value);
	}
</script>

<div class="space-y-3 border-t border-border/40 pt-3">
	<span class="text-sm font-medium text-muted-foreground">Placement requirements</span>

	{#if constraints.length === 0}
		<p class="text-sm italic text-muted-foreground">
			No constraints — matches any runner in the group.
		</p>
	{:else}
		<div class="space-y-3">
			{#each constraints as c, idx (idx)}
				<div class="space-y-2 rounded-md border border-border/60 p-2">
					<!-- Capability picker -->
					<FormField label="Capability" for={`req-cap-${idx}`}>
						<Select.Root
							type="single"
							value={c.capability}
							onValueChange={(v) => {
								if (v) setCapability(idx, v);
							}}
							disabled={readonly}
						>
							<Select.Trigger id={`req-cap-${idx}`} class="w-full" disabled={readonly}>
								{c.capability || 'Select capability…'}
							</Select.Trigger>
							<Select.Content>
								{#if capabilityTypes.length === 0 && capTypesLoaded}
									<Select.Item value="" label="No capability types defined" />
								{/if}
								{#each capabilityTypes as ct (ct.id)}
									<Select.Item value={ct.name} label={ct.name} />
								{/each}
							</Select.Content>
						</Select.Root>
					</FormField>

					<!-- Field picker (driven by selected capability) -->
					<FormField label="Field" for={`req-field-${idx}`}>
						<Select.Root
							type="single"
							value={c.field}
							onValueChange={(v) => {
								if (v) setField(idx, v);
							}}
							disabled={readonly || !c.capability}
						>
							<Select.Trigger
								id={`req-field-${idx}`}
								class="w-full"
								disabled={readonly || !c.capability}
							>
								{c.field || 'Select field…'}
							</Select.Trigger>
							<Select.Content>
								{#each fieldsForCap(c.capability) as f (f)}
									<Select.Item value={f} label={f} />
								{/each}
							</Select.Content>
						</Select.Root>
					</FormField>

					<!-- Op picker -->
					<FormField label="Operator" for={`req-op-${idx}`}>
						<Select.Root
							type="single"
							value={c.op}
							onValueChange={(v) => {
								if (v) setOp(idx, v as ConstraintOp);
							}}
							disabled={readonly}
						>
							<Select.Trigger id={`req-op-${idx}`} class="w-full" disabled={readonly}>
								{OP_LABELS[c.op] ?? c.op}
							</Select.Trigger>
							<Select.Content>
								{#each ALL_OPS as op (op)}
									<Select.Item value={op} label={OP_LABELS[op]} />
								{/each}
							</Select.Content>
						</Select.Root>
					</FormField>

					<!-- Value input — hidden for 'exists', type-aware otherwise -->
					{#if OPS_WITH_VALUE.includes(c.op)}
						{@const kind = kindForField(c.capability, c.field)}
						{@const selectOpts = optionsForField(c.capability, c.field)}

						{#if kind === 'bool'}
							<FormField label="Value" for={`req-val-${idx}`}>
								<Select.Root
									type="single"
									value={displayValue(c.value)}
									onValueChange={(v) => {
										if (v !== undefined) patchConstraint(idx, { value: v === 'true' });
									}}
									disabled={readonly}
								>
									<Select.Trigger id={`req-val-${idx}`} class="w-full" disabled={readonly}>
										{displayValue(c.value) || 'Select…'}
									</Select.Trigger>
									<Select.Content>
										<Select.Item value="true" label="true" />
										<Select.Item value="false" label="false" />
									</Select.Content>
								</Select.Root>
							</FormField>
						{:else if kind === 'select' && selectOpts.length > 0 && c.op !== 'in'}
							<FormField label="Value" for={`req-val-${idx}`}>
								<Select.Root
									type="single"
									value={displayValue(c.value)}
									onValueChange={(v) => {
										if (v !== undefined) patchConstraint(idx, { value: v });
									}}
									disabled={readonly}
								>
									<Select.Trigger id={`req-val-${idx}`} class="w-full" disabled={readonly}>
										{displayValue(c.value) || 'Select…'}
									</Select.Trigger>
									<Select.Content>
										{#each selectOpts as opt (opt)}
											<Select.Item value={opt} label={opt} />
										{/each}
									</Select.Content>
								</Select.Root>
							</FormField>
						{:else if kind === 'number' && c.op !== 'in'}
							<FormField label="Value" for={`req-val-${idx}`}>
								<Input
									id={`req-val-${idx}`}
									type="number"
									value={displayValue(c.value)}
									disabled={readonly}
									oninput={(e) =>
										patchConstraint(idx, {
											value: parseValue((e.currentTarget as HTMLInputElement).value, 'number')
										})}
								/>
							</FormField>
						{:else}
							<!-- text / textarea / timestamp / json / in / fallback -->
							<FormField
								label={c.op === 'in' ? 'Values (comma-separated)' : 'Value'}
								for={`req-val-${idx}`}
							>
								<Input
									id={`req-val-${idx}`}
									type="text"
									class="font-mono text-sm"
									placeholder={c.op === 'in' ? 'a, b, c' : ''}
									value={c.op === 'in'
										? Array.isArray(c.value)
											? (c.value as string[]).join(', ')
											: displayValue(c.value)
										: displayValue(c.value)}
									disabled={readonly}
									oninput={(e) => {
										const raw = (e.currentTarget as HTMLInputElement).value;
										if (c.op === 'in') {
											patchConstraint(idx, {
												value: raw
													.split(',')
													.map((s) => s.trim())
													.filter(Boolean)
											});
										} else {
											patchConstraint(idx, { value: parseValue(raw, kind) });
										}
									}}
								/>
							</FormField>
						{/if}
					{/if}

					<!-- Remove row -->
					{#if !readonly}
						<div class="flex justify-end">
							<Button
								variant="ghost"
								size="sm"
								class="h-7 px-2 text-sm text-destructive hover:text-destructive"
								onclick={() => removeConstraint(idx)}
							>
								Remove
							</Button>
						</div>
					{/if}
				</div>
			{/each}
		</div>
	{/if}

	{#if !readonly}
		<Button
			variant="outline"
			size="sm"
			class="h-7 gap-1 px-2 text-sm"
			onclick={addConstraint}
			disabled={capabilityTypes.length === 0 && capTypesLoaded}
		>
			Add constraint
		</Button>
		{#if capabilityTypes.length === 0 && capTypesLoaded}
			<p class="text-sm italic text-muted-foreground">
				No capability types defined in this workspace. Add them under
				<code class="font-mono">/admin/capability-types</code> first.
			</p>
		{/if}
	{/if}

	<p class="text-sm italic text-muted-foreground">
		Constraints are AND-ed. Only runners whose advertised capabilities satisfy all constraints can
		claim this step. Applies to runner-group steps only — concurrency-limit steps ignore requirements.
	</p>
</div>
