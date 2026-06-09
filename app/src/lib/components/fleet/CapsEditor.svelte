<script lang="ts">
	// Registry-driven typed editor for a human roster member's admin-assigned
	// `caps` blob. The caps shape mirrors a runner's advertised `capabilities`
	// JSONB and is what the engine's `satisfies(requirements, caps)` matcher and
	// the backend `validate_caps_against_types` gate read — a TWO-LEVEL bag:
	//
	//   { "<capability_name>": { "<field>": <value>, … }, … }
	//
	// e.g. `{ "xrd": { "max_2theta": 188, "source": "dev-diffractometer" }, "docker": {} }`.
	//
	// So each row is a CAPABILITY TYPE (picked from the workspace registry's
	// declared types), and the row renders that type's declared fields as typed
	// inputs (widget chosen by each field's `FieldKind`). A type with no fields is
	// a bare presence capability (value `{}`). The empty bag is valid. The backend
	// re-validates on save and returns 400 on mismatch; the parent surfaces that.
	import { onMount } from 'svelte';
	import * as Select from '$lib/components/ui/select';
	import { Input } from '$lib/components/ui/input';
	import { Button } from '$lib/components/ui/button';
	import { Checkbox } from '$lib/components/ui/checkbox';
	import { FormField } from '$lib/components/ui/form-field';
	import {
		listCapabilityTypes,
		type CapabilityTypeSummary,
		type CapabilityField
	} from '$lib/api/capability-types';

	type Props = {
		value: Record<string, unknown>;
		onchange: (next: Record<string, unknown>) => void;
	};

	let { value, onchange }: Props = $props();

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

	// ── Row model ───────────────────────────────────────────────────────────────
	// One row per assigned capability. The row order is derived from the bag's
	// keys so external updates re-flow in; each row's nested field object lives at
	// `value[capName]`.
	const assigned = $derived<string[]>(Object.keys(value));

	const typeNames = $derived<string[]>(capabilityTypes.map((c) => c.name));

	/** The registry descriptor for a capability name, if known. */
	function typeFor(cap: string): CapabilityTypeSummary | undefined {
		return capabilityTypes.find((c) => c.name === cap);
	}

	/** The nested field object currently stored for a capability (defensive). */
	function fieldsOf(cap: string): Record<string, unknown> {
		const v = value[cap];
		return v && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : {};
	}

	// ── Mutation helpers ─────────────────────────────────────────────────────────

	function emit(next: Record<string, unknown>) {
		onchange(next);
	}

	/** Seed a type's declared fields with sensible defaults (so REQUIRED fields are
	 *  present and the enroll-time validator passes). */
	function seedFields(cap: string): Record<string, unknown> {
		const t = typeFor(cap);
		const f: Record<string, unknown> = {};
		for (const fld of t?.fields ?? []) f[fld.name] = defaultForField(fld);
		return f;
	}

	function addCapability() {
		const next = typeNames.find((n) => !(n in value));
		const cap = next ?? '';
		if (!cap) return; // nothing left to add (or no registry)
		emit({ ...value, [cap]: seedFields(cap) });
	}

	function removeCapability(cap: string) {
		const next = { ...value };
		delete next[cap];
		emit(next);
	}

	/** Rename a capability key (re-seeds the new type's fields). */
	function setCapability(oldCap: string, newCap: string) {
		if (newCap === oldCap) return;
		const next: Record<string, unknown> = {};
		// Preserve order: walk the existing keys, swapping the renamed one in place.
		for (const k of Object.keys(value)) {
			if (k === oldCap) next[newCap] = seedFields(newCap);
			else next[k] = value[k];
		}
		emit(next);
	}

	function setField(cap: string, field: string, fieldValue: unknown) {
		emit({ ...value, [cap]: { ...fieldsOf(cap), [field]: fieldValue } });
	}

	/** A starting value for a declared field, by kind (Select → first option). */
	function defaultForField(field: CapabilityField): unknown {
		if (field.kind === 'bool') return false;
		if (field.kind === 'number') return 0;
		if (field.kind === 'select') return field.options?.[0] ?? '';
		return '';
	}

	function parseValue(raw: string, kind: string): unknown {
		if (kind === 'number') {
			const n = parseFloat(raw);
			return isNaN(n) ? raw : n;
		}
		return raw;
	}

	function displayValue(v: unknown): string {
		if (v === undefined || v === null) return '';
		return String(v);
	}
</script>

<div class="space-y-3">
	{#if assigned.length === 0}
		<p class="text-sm italic text-muted-foreground">
			No capabilities assigned — this member matches steps with no placement requirements.
		</p>
	{:else}
		<div class="space-y-3">
			{#each assigned as cap (cap)}
				{@const t = typeFor(cap)}
				{@const fields = t?.fields ?? []}
				<div class="space-y-2 rounded-md border border-border/60 p-2">
					<!-- Capability type — picked from the workspace registry -->
					<FormField label="Capability" for={`cap-${cap}`}>
						{#if typeNames.length > 0}
							<Select.Root
								type="single"
								value={cap}
								onValueChange={(v) => {
									if (v !== undefined) setCapability(cap, v);
								}}
							>
								<Select.Trigger id={`cap-${cap}`} class="w-full font-mono text-sm">
									{cap || 'Select a capability…'}
								</Select.Trigger>
								<Select.Content>
									{#each typeNames as name (name)}
										<!-- allow re-selecting the current cap; hide ones already assigned -->
										{#if name === cap || !(name in value)}
											<Select.Item value={name} label={name} />
										{/if}
									{/each}
								</Select.Content>
							</Select.Root>
						{:else}
							<Input id={`cap-${cap}`} class="font-mono text-sm" value={cap} readonly />
						{/if}
					</FormField>

					<!-- Declared fields for this capability type -->
					{#if fields.length === 0}
						<p class="text-xs italic text-muted-foreground">
							Presence capability — no fields to configure.
						</p>
					{:else}
						{#each fields as field (field.name)}
							{@const fv = fieldsOf(cap)[field.name]}
							{#if field.kind === 'bool'}
								<FormField label={field.name} for={`cap-${cap}-${field.name}`}>
									<div class="flex h-9 items-center">
										<Checkbox
											id={`cap-${cap}-${field.name}`}
											checked={fv === true}
											onCheckedChange={(c) => setField(cap, field.name, c === true)}
										/>
									</div>
								</FormField>
							{:else if field.kind === 'select' && (field.options?.length ?? 0) > 0}
								<FormField label={field.name} for={`cap-${cap}-${field.name}`}>
									<Select.Root
										type="single"
										value={displayValue(fv)}
										onValueChange={(v) => {
											if (v !== undefined) setField(cap, field.name, v);
										}}
									>
										<Select.Trigger id={`cap-${cap}-${field.name}`} class="w-full">
											{displayValue(fv) || 'Select…'}
										</Select.Trigger>
										<Select.Content>
											{#each field.options ?? [] as opt (opt)}
												<Select.Item value={opt} label={opt} />
											{/each}
										</Select.Content>
									</Select.Root>
								</FormField>
							{:else if field.kind === 'number'}
								<FormField label={field.name} for={`cap-${cap}-${field.name}`}>
									<Input
										id={`cap-${cap}-${field.name}`}
										type="number"
										value={displayValue(fv)}
										oninput={(e) =>
											setField(
												cap,
												field.name,
												parseValue((e.currentTarget as HTMLInputElement).value, 'number')
											)}
									/>
								</FormField>
							{:else}
								<FormField label={field.name} for={`cap-${cap}-${field.name}`}>
									<Input
										id={`cap-${cap}-${field.name}`}
										type="text"
										class="font-mono text-sm"
										value={displayValue(fv)}
										oninput={(e) =>
											setField(cap, field.name, (e.currentTarget as HTMLInputElement).value)}
									/>
								</FormField>
							{/if}
						{/each}
					{/if}

					<div class="flex justify-end">
						<Button
							variant="ghost"
							size="sm"
							class="h-7 px-2 text-sm text-destructive hover:text-destructive"
							onclick={() => removeCapability(cap)}
						>
							Remove
						</Button>
					</div>
				</div>
			{/each}
		</div>
	{/if}

	<Button
		variant="outline"
		size="sm"
		class="h-7 gap-1 px-2 text-sm"
		disabled={capTypesLoaded && typeNames.every((n) => n in value)}
		onclick={addCapability}
	>
		Add capability
	</Button>

	{#if capabilityTypes.length === 0 && capTypesLoaded}
		<p class="text-sm italic text-muted-foreground">
			No capability types defined in this workspace. Define them under
			<code class="font-mono">/admin/capability-types</code> to assign typed capabilities here.
		</p>
	{/if}
</div>
