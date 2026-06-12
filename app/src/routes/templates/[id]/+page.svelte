<script lang="ts">
	import { page } from '$app/state';
	import TemplateEditor from '$lib/components/editor/TemplateEditor.svelte';

	const templateId = $derived(page.params.id!);
</script>

<!-- SvelteKit reuses this component across /templates/A → /templates/B, so
     the Yjs-session-owning editor is keyed on the route param: a version
     switch (or owner-breadcrumb nav) destroys the old editor — closing its
     /api/yjs WebSocket via releaseSession — and mounts a fresh one, exactly
     like a cold load, without a full document reload. -->
{#key templateId}
	<TemplateEditor {templateId} />
{/key}
