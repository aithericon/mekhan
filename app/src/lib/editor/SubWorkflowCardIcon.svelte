<script lang="ts" module>
	import { getContext, setContext } from 'svelte';

	const KEY = Symbol('sub-workflow-card-icon-token');

	/** Publish the icon token a downstream {@link SubWorkflowCardIcon} should
	 *  render. SubWorkflowNode sets this so the icon component handed to
	 *  WorkflowNodeCard (which can only pass `class`) can still see the token. */
	export function setSubWorkflowIconToken(get: () => string | null | undefined): void {
		setContext(KEY, get);
	}
</script>

<script lang="ts">
	// Adapter so an `asset:`-backed (uploaded logo) icon can flow through
	// WorkflowNodeCard's `icon: Component<{ class?: string }>` slot, which only
	// passes a `class`. The icon token is read from context (published by the
	// parent SubWorkflowNode via `setSubWorkflowIconToken`); rendering is delegated
	// to the shared NodeIcon (named-registry glyph OR fetched <img> for assets).
	import NodeIcon from './NodeIcon.svelte';

	let { class: className }: { class?: string } = $props();

	const getToken = getContext<(() => string | null | undefined) | undefined>(KEY);
	const token = $derived(getToken ? getToken() : undefined);
</script>

<NodeIcon icon={token} class={className} />
