/**
 * Shared port comparison utility.
 *
 * `portsEqual` does a structural comparison of two Port values, checking
 * id, label, and all field metadata. Used by the debounced-derive effects in
 * AutomatedStepSection and SubWorkflowSection to avoid emitting a no-op
 * `onchange` when the fetched port is identical to the one already stored.
 */
import type { components } from '$lib/api/schema';

type Port = components['schemas']['Port'];

/**
 * Return `true` when `a` and `b` are structurally identical — same id, label,
 * and an ordered-equal fields array.
 *
 * AutomatedStepSection checks all five field attributes (name, kind, label,
 * required, description). SubWorkflowSection only needs name + kind because
 * the child contract only surfaces those. This implementation checks all five
 * so it is safe for both callers; surplus equality is never wrong.
 */
export function portsEqual(a: Port | undefined, b: Port): boolean {
	if (!a) return false;
	if (a.id !== b.id || a.label !== b.label) return false;
	const af = a.fields ?? [];
	const bf = b.fields ?? [];
	if (af.length !== bf.length) return false;
	for (let i = 0; i < af.length; i++) {
		const x = af[i];
		const y = bf[i];
		if (
			x.name !== y.name ||
			x.kind !== y.kind ||
			x.label !== y.label ||
			(x.required ?? false) !== (y.required ?? false) ||
			(x.description ?? null) !== (y.description ?? null)
		) {
			return false;
		}
	}
	return true;
}
