<script lang="ts">
	// SPDX-License-Identifier: Apache-2.0
	import type { TaskBlock } from '../../types';

	// eslint-disable-next-line @typescript-eslint/no-unused-vars
	let { block: _block }: { block: Extract<TaskBlock, { type: 'divider' }> } = $props();
</script>

<hr class="my-4 border-border/50" data-testid="step-block-divider" />
