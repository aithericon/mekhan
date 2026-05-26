<script lang="ts">
	import {
		Sheet,
		SheetContent,
		SheetTitle,
		SheetDescription
	} from '$lib/components/ui/sheet';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { Label } from '$lib/components/ui/label';
	import { promoteInstanceToTest } from '$lib/api/client';
	import { goto } from '$app/navigation';

	type Props = {
		open: boolean;
		instanceId: string;
		templateId: string;
		onclose: () => void;
	};

	let { open, instanceId, templateId, onclose }: Props = $props();

	let name = $state('');
	let saving = $state(false);
	let error = $state<string | null>(null);

	$effect(() => {
		if (!open) {
			name = '';
			error = null;
		}
	});

	async function handleSave() {
		if (!name.trim()) return;
		saving = true;
		error = null;
		try {
			const test = await promoteInstanceToTest(instanceId, { name: name.trim() });
			onclose();
			// Open the test editor on the new test row so the user can add
			// assertions immediately.
			await goto(`/templates/${templateId}?test=${test.id}`);
		} catch (e) {
			error = e instanceof Error ? e.message : 'failed to promote';
		} finally {
			saving = false;
		}
	}
</script>

<Sheet.Root
	{open}
	onOpenChange={(o: boolean) => {
		if (!o) onclose();
	}}
>
	<SheetContent class="flex w-full max-w-md flex-col gap-0 p-0 sm:max-w-md">
		<header class="border-b border-border px-5 py-4">
			<SheetTitle>Save as test</SheetTitle>
			<SheetDescription class="text-sm text-muted-foreground">
				Scoops this instance's start tokens and human-task answers into a new
				template test. You add assertions afterward in the template editor.
			</SheetDescription>
		</header>

		<div class="flex-1 space-y-3 px-5 py-4 text-sm">
			{#if error}
				<div
					class="rounded border border-red-200 bg-red-50 px-3 py-2 text-red-800"
				>
					{error}
				</div>
			{/if}
			<div class="space-y-1">
				<Label for="test-name-promote">Test name</Label>
				<Input
					id="test-name-promote"
					bind:value={name}
					placeholder="happy-path"
					autofocus
				/>
				<p class="text-xs text-muted-foreground">
					Must be unique within this template family.
				</p>
			</div>
		</div>

		<footer class="flex justify-end gap-2 border-t border-border px-5 py-3">
			<Button variant="outline" onclick={onclose} disabled={saving}>Cancel</Button>
			<Button onclick={handleSave} disabled={saving || !name.trim()}>
				{saving ? 'Saving…' : 'Save & edit'}
			</Button>
		</footer>
	</SheetContent>
</Sheet.Root>
