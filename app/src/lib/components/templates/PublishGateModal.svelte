<script lang="ts">
	import {
		Sheet,
		SheetContent,
		SheetTitle,
		SheetDescription
	} from '$lib/components/ui/sheet';
	import { Button } from '$lib/components/ui/button';
	import AlertTriangle from '@lucide/svelte/icons/alert-triangle';
	import type { FailingTestInfo } from '$lib/api/client';

	type Props = {
		open: boolean;
		failingTests: FailingTestInfo[];
		onclose: () => void;
		onretry: () => void;
		onforce: () => void;
	};

	let { open, failingTests, onclose, onretry, onforce }: Props = $props();

	let confirmingForce = $state(false);
</script>

<Sheet.Root
	{open}
	onOpenChange={(o: boolean) => {
		if (!o) onclose();
	}}
>
	<SheetContent class="flex w-full max-w-lg flex-col gap-0 p-0 sm:max-w-lg">
		<header class="flex items-start gap-3 border-b border-border px-5 py-4">
			<AlertTriangle class="size-5 text-amber-600" />
			<div>
				<SheetTitle>Publish blocked by failing tests</SheetTitle>
				<SheetDescription class="text-sm text-muted-foreground">
					{failingTests.length} enabled test{failingTests.length === 1 ? '' : 's'}
					did not pass against this version.
				</SheetDescription>
			</div>
		</header>

		<div class="flex-1 overflow-y-auto px-5 py-4 text-sm">
			<ul class="space-y-2">
				{#each failingTests as t}
					<li class="rounded border border-border p-3" data-testid="failing-test">
						<div class="font-medium">{t.name}</div>
						<div class="text-xs text-muted-foreground">{t.reason}</div>
					</li>
				{/each}
			</ul>

			{#if confirmingForce}
				<div
					class="mt-4 rounded border border-amber-200 bg-amber-50 p-3 text-amber-900"
				>
					<p class="font-medium">Force publish anyway?</p>
					<p class="mt-1 text-xs">
						The template will be published with failing tests on record. The bypass
						is logged for audit.
					</p>
				</div>
			{/if}
		</div>

		<footer class="flex justify-end gap-2 border-t border-border px-5 py-3">
			<Button variant="outline" onclick={onclose}>Close</Button>
			<Button variant="outline" onclick={onretry}>Run again</Button>
			{#if !confirmingForce}
				<Button
					variant="destructive"
					onclick={() => (confirmingForce = true)}
				>
					Force publish…
				</Button>
			{:else}
				<Button variant="destructive" onclick={onforce}>
					Force publish (override)
				</Button>
			{/if}
		</footer>
	</SheetContent>
</Sheet.Root>
