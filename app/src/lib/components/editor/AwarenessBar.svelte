<script lang="ts">
	import { onDestroy } from 'svelte';
	import type { Awareness } from 'y-protocols/awareness';
	import { onRemoteChange, type UserPresence } from '$lib/yjs/awareness';

	type Props = {
		awareness: Awareness;
	};

	let { awareness }: Props = $props();

	let remoteUsers = $state<UserPresence[]>([]);

	// One-time subscription to the awareness instance handed in at mount; the
	// prop is stable for the component's life, so the initial-value read is intended.
	// svelte-ignore state_referenced_locally
	const unsubscribe = onRemoteChange(awareness, (users) => {
		remoteUsers = users;
	});

	onDestroy(unsubscribe);
</script>

{#if remoteUsers.length > 0}
	<div class="flex items-center gap-1">
		{#each remoteUsers as user}
			<div
				class="flex size-5 items-center justify-center rounded-full text-sm font-bold text-white"
				style="background-color: {user.color}"
				title={user.name}
			>
				{user.name?.charAt(0).toUpperCase() ?? '?'}
			</div>
		{/each}
	</div>
{/if}
