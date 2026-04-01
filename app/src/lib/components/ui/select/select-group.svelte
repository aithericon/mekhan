<script lang="ts">
	// SPDX-License-Identifier: Apache-2.0
	import { Select as SelectPrimitive } from 'bits-ui';

	let { ref = $bindable(null), ...restProps }: SelectPrimitive.GroupProps = $props();
</script>

<SelectPrimitive.Group bind:ref data-slot="select-group" {...restProps} />
