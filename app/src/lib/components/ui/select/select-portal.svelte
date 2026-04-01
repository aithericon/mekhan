<script lang="ts">
	// SPDX-License-Identifier: Apache-2.0
	import { Select as SelectPrimitive } from 'bits-ui';

	let { ...restProps }: SelectPrimitive.PortalProps = $props();
</script>

<SelectPrimitive.Portal {...restProps} />
