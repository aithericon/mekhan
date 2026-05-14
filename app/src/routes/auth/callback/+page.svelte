<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { auth } from '$lib/auth/store.svelte';
	import { ensureAuthInitialized } from '$lib/auth/guard';

	let error = $state<string | null>(null);

	onMount(async () => {
		try {
			await ensureAuthInitialized();
			await auth.completeSignIn();
			await goto('/', { replaceState: true });
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		}
	});
</script>

<div class="flex h-screen items-center justify-center">
	{#if error}
		<div class="rounded border border-destructive bg-destructive/10 p-4 text-sm">
			<div class="font-medium text-destructive">Sign-in failed</div>
			<div class="mt-1 text-muted-foreground">{error}</div>
		</div>
	{:else}
		<div class="text-sm text-muted-foreground">Finishing sign-in…</div>
	{/if}
</div>
