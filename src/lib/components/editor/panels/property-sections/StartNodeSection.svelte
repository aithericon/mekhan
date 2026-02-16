<script lang="ts">
	import type { StartNodeData } from '$lib/types/editor';
	import KeyValueEditor from '../shared/KeyValueEditor.svelte';

	type Props = {
		data: StartNodeData;
		readonly?: boolean;
		onchange: (data: StartNodeData) => void;
	};

	let { data, readonly = false, onchange }: Props = $props();

	function handleInitialDataChange(entries: Record<string, unknown>) {
		onchange({ ...data, initialData: entries });
	}
</script>

<div class="space-y-1.5">
	<span class="text-xs font-medium text-muted-foreground">Initial Data</span>
	<KeyValueEditor
		entries={data.initialData ?? {}}
		{readonly}
		keyPlaceholder="Variable"
		valuePlaceholder="Value"
		onchange={handleInitialDataChange}
	/>
</div>
