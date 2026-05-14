<script lang="ts" module>
	import type { BadgeVariant, BadgeSize } from "$lib/components/ui/badge";

	export type NodeKind =
		// Place kinds
		| "place"
		| "signal"
		| "bridge_out"
		| "bridge_in"
		| "bridge_reply"
		// Transition kinds
		| "effect"
		| "rhai"
		// Event types
		| "TransitionFired"
		| "EffectCompleted"
		| "EffectFailed"
		| "TokenCreated"
		| "TokenConsumed"
		| "TokenBridgedOut"
		| "NetInitialized"
		| "ErrorOccurred"
		// Coordination signal types
		| "accepted"
		| "denied"
		| "confirmed"
		| "failed"
		// Generic
		| "remote_net"
		| "group"
		| "lease";

	type KindSpec = { variant: BadgeVariant; label: string };

	// Single source of truth for badge variant + label per petri concept.
	// Keeps domain-specific marker colors out of consumer components, which
	// can use semantic Badge variants instead of literal Tailwind palette colors.
	const KIND_MAP: Record<NodeKind, KindSpec> = {
		// Place kinds — map to functional roles
		place:         { variant: "info",        label: "internal" },
		signal:        { variant: "warning",     label: "signal" },
		bridge_out:    { variant: "destructive", label: "bridge out" },
		bridge_in:     { variant: "success",     label: "bridge in" },
		bridge_reply:  { variant: "secondary",   label: "bridge reply" },

		// Transition kinds
		effect: { variant: "secondary", label: "Effect" },
		rhai:   { variant: "info",      label: "Rhai Script" },

		// Event types — map to outcome semantics
		TransitionFired:  { variant: "success",     label: "TransitionFired" },
		EffectCompleted:  { variant: "success",     label: "EffectCompleted" },
		EffectFailed:     { variant: "destructive", label: "EffectFailed" },
		TokenCreated:     { variant: "info",        label: "TokenCreated" },
		TokenConsumed:    { variant: "muted",       label: "TokenConsumed" },
		TokenBridgedOut:  { variant: "warning",     label: "TokenBridgedOut" },
		NetInitialized:   { variant: "info",        label: "NetInitialized" },
		ErrorOccurred:    { variant: "destructive", label: "ErrorOccurred" },

		// Coordination signals
		accepted:  { variant: "success",     label: "accepted" },
		denied:    { variant: "destructive", label: "denied" },
		confirmed: { variant: "info",        label: "confirmed" },
		failed:    { variant: "destructive", label: "failed" },

		// Generic
		remote_net: { variant: "success",   label: "Remote Net" },
		group:      { variant: "default",   label: "Group" },
		lease:      { variant: "warning",   label: "Lease" },
	};

	export function nodeKindSpec(kind: NodeKind): KindSpec {
		return KIND_MAP[kind];
	}
</script>

<script lang="ts">
	import { Badge } from "$lib/components/ui/badge";

	let {
		kind,
		label,
		size = "sm",
		class: className,
	}: {
		kind: NodeKind;
		label?: string;
		size?: BadgeSize;
		class?: string;
	} = $props();

	const spec = $derived(KIND_MAP[kind]);
</script>

<Badge variant={spec.variant} {size} class={className}>
	{label ?? spec.label}
</Badge>
