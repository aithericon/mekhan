<!--
  /pages/[id] — full-screen route for a free page. Thin {#key} wrapper: a new
  page id fully remounts <PageEditorScreen> (fresh Y.Doc + provider).
-->
<script lang="ts">
	import { page } from '$app/state';
	import PageEditorScreen from '$lib/components/pages/PageEditorScreen.svelte';

	const id = $derived(page.params.id!);
</script>

{#key id}
	<PageEditorScreen pageId={id} />
{/key}
