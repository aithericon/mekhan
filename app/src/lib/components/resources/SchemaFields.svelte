<script lang="ts" module>
	// Reusable schema-driven field block. Extracted verbatim from the inline
	// `deriveFieldSpecs` + `discriminatorOf` + `SchemaForm` wiring that lived in
	// `ResourceEditModal.svelte`, so the resource create/edit modal AND the
	// Control-Plane's NewCapacityModal (cluster/datacenter branch) render their
	// typed config the same way and can't drift.
	//
	// Given a resource-type `descriptor` (the `ResourceTypeInfo` from
	// `GET /api/v1/resources/types`), it renders the bordered "<type> configuration"
	// block: one widget per JSON-Schema property (discriminator select + only the
	// active variant's fields for a `oneOf`-discriminated schema like a datacenter's
	// `scheduler_flavor`). The `fieldValues` map is bindable (string model — booleans
	// / numbers are coerced at submit by the caller's `buildConfig`); the resolved
	// `discriminator` field name is bindable too so the caller can read the chosen
	// flavor.
	import {
		deriveFieldSpecs as _deriveFieldSpecs,
		discriminatorOf as _discriminatorOf,
		type FieldSpec
	} from '$lib/components/editor/panels/shared/SchemaForm.svelte';

	// Re-export so existing importers (ResourceEditModal's buildConfig) can pull the
	// derivation helpers from one place alongside this component.
	export const deriveFieldSpecs = _deriveFieldSpecs;
	export const discriminatorOf = _discriminatorOf;
	export type { FieldSpec };
</script>

<script lang="ts">
	import SchemaForm from '$lib/components/editor/panels/shared/SchemaForm.svelte';
	import type { ResourceTypeInfo } from '$lib/api/resources';

	type Props = {
		/** The resource-type descriptor whose `schema` is rendered. */
		descriptor: ResourceTypeInfo | null;
		/** Bindable string-model config map (field name → raw string value). */
		fieldValues: Record<string, string>;
		/** Bindable resolved discriminator field name (`null` for a plain object
		 *  schema). Exposed so the caller can read the chosen flavor value via
		 *  `fieldValues[discriminator]`. */
		discriminator?: string | null;
		/** Placeholder shown on secret inputs (e.g. "(leave blank to keep
		 *  current)" in the resource edit modal). */
		secretPlaceholder?: string;
		/** When false, hide the bordered wrapper + header (caller frames it). */
		framed?: boolean;
	};

	let {
		descriptor,
		fieldValues = $bindable(),
		discriminator = $bindable(null),
		secretPlaceholder,
		framed = true
	}: Props = $props();

	// Discriminator field (e.g. a datacenter's `scheduler_flavor`) for a
	// `oneOf`-discriminated schema; `null` for a plain object schema. Kept in sync
	// with the bound `discriminator` prop so the caller can read the chosen flavor.
	const resolvedDiscriminator = $derived(
		descriptor ? discriminatorOf(descriptor.schema as Record<string, unknown>) : null
	);
	$effect(() => {
		discriminator = resolvedDiscriminator;
	});
</script>

{#if descriptor}
	{#if framed}
		<div class="space-y-3 rounded-md border border-border/60 p-3">
			<div class="text-sm font-medium text-muted-foreground">
				{descriptor.display_name} configuration
			</div>
			<SchemaForm
				schema={(descriptor.schema ?? {}) as Record<string, unknown>}
				value={fieldValues}
				secretFields={descriptor.secret_fields}
				fieldOrder={[...descriptor.public_fields, ...descriptor.secret_fields]}
				booleanWidget="select"
				{secretPlaceholder}
				onchange={(next) => (fieldValues = next as Record<string, string>)}
			/>
		</div>
	{:else}
		<SchemaForm
			schema={(descriptor.schema ?? {}) as Record<string, unknown>}
			value={fieldValues}
			secretFields={descriptor.secret_fields}
			fieldOrder={[...descriptor.public_fields, ...descriptor.secret_fields]}
			booleanWidget="select"
			{secretPlaceholder}
			onchange={(next) => (fieldValues = next as Record<string, string>)}
		/>
	{/if}
{/if}
