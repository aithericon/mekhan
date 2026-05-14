<script lang="ts">
	import type { StartNodeData } from '$lib/types/editor';
	import type { components } from '$lib/api/schema';
	import PortsSection from './PortsSection.svelte';

	type Port = components['schemas']['Port'];

	type Props = {
		data: StartNodeData;
		readonly?: boolean;
		onchange: (data: StartNodeData) => void;
	};

	let { data, readonly = false, onchange }: Props = $props();

	// Pre-typed-ports templates have no `initial` field — synthesize an empty
	// input port so the editor renders cleanly.
	const initial: Port = $derived(
		data.initial ?? { id: 'in', label: 'Input', fields: [] }
	);

	function handlePortChange(port: Port) {
		onchange({ ...data, initial: port });
	}
</script>

<PortsSection
	port={initial}
	{readonly}
	title="Initial token fields"
	emptyHint="No initial fields. Instances of this template will start with an empty token (system fields only). Add fields to require typed input at instance creation."
	onchange={handlePortChange}
/>
