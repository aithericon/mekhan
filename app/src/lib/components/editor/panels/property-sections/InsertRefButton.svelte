<script lang="ts">
	// Compact "Insert variable" affordance that wraps the same two-column
	// RefPicker the Decision branch editor uses. On pick, hands back a ready-
	// to-drop `{{ … }}` snippet: borrowed refs (`<slug>.<field>`) go in as-is
	// (the runtime plucks against the inbound token), and control-token
	// leaves (`input.<path>`) have the implicit `input.` prefix stripped —
	// otherwise `placeholder_to_accessor` would resolve `input.x` to
	// `input["input"]["x"]`.
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import RefPicker from './RefPicker.svelte';

	type Props = {
		scope: ScopeEntry[];
		disabled?: boolean;
		placeholder?: string;
		triggerClass?: string;
		/** Called with the `{{ … }}` snippet ready to insert/append. */
		oninsert: (snippet: string) => void;
	};

	let {
		scope,
		disabled = false,
		placeholder = 'Insert variable…',
		triggerClass = '',
		oninsert
	}: Props = $props();

	function refToInterpolation(qualified: string): string {
		const stripped = qualified.startsWith('input.')
			? qualified.slice('input.'.length)
			: qualified;
		return `{{ ${stripped} }}`;
	}
</script>

<RefPicker
	{scope}
	{disabled}
	{placeholder}
	{triggerClass}
	onpick={(e) => oninsert(refToInterpolation(e.qualified))}
/>
