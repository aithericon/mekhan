<script lang="ts">
	import { page } from '$app/state';
	import IdeWorkbench from '$lib/components/ide/IdeWorkbench.svelte';

	const templateId = $derived(page.params.id!);
</script>

<!-- Same keying as /templates/[id]: SvelteKit reuses this component across
     param-only navs, so the Yjs-session-owning workbench is keyed on the
     route param — a version switch tears the old session down (WS closed via
     releaseSession) and mounts a fresh one without a full document reload. -->
{#key templateId}
	<IdeWorkbench {templateId} />
{/key}
