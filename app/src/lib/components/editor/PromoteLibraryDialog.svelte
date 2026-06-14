<!--
  PromoteLibraryDialog — Phase 4 governance. Promote a published template to a
  workspace library node, or re-brand one that already is. Mirrors
  CreateWorkspaceDialog's shadcn form shape. Category options come from the
  backend vocab endpoint so the picker can never drift from server validation;
  icon options come from the frontend icon registry (decision 9).
-->
<script lang="ts">
	import Package from '@lucide/svelte/icons/package';
	import {
		Dialog,
		DialogContent,
		DialogHeader,
		DialogTitle,
		DialogDescription,
		DialogFooter
	} from '$lib/components/ui/dialog';
	import * as Select from '$lib/components/ui/select';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { Label } from '$lib/components/ui/label';
	import {
		ApiError,
		promoteTemplate,
		listLibraryCategories,
		type Template
	} from '$lib/api/client';
	import { resolveNodeIcon, iconRegistryKeys } from '$lib/editor/icon-registry';

	let {
		open = $bindable(false),
		template,
		onpromoted
	}: {
		open?: boolean;
		template: Template;
		onpromoted?: (t: Template) => void;
	} = $props();

	const iconKeys = iconRegistryKeys();
	let categories = $state<string[]>([]);

	let coordinate = $state('');
	let vendor = $state('');
	let category = $state('');
	let icon = $state('');
	let color = $state('#14b8a6');
	let badge = $state('');
	let submitting = $state(false);
	let error = $state<string | null>(null);

	const isManage = $derived(template?.template_kind === 'library_node');
	const PreviewIcon = $derived(resolveNodeIcon(icon));

	// Re-seed the form from the template each time the dialog opens, and lazily
	// fetch the category vocabulary once.
	$effect(() => {
		if (!open) return;
		const p = (template?.presentation ?? {}) as Record<string, string | undefined>;
		coordinate = template?.coordinate ?? '';
		vendor = p.vendor ?? '';
		category = p.category ?? '';
		icon = p.icon ?? '';
		color = p.color ?? '#14b8a6';
		badge = p.badge ?? '';
		error = null;
		submitting = false;
		if (categories.length === 0) {
			listLibraryCategories()
				.then((c) => (categories = c))
				.catch(() => {
					/* leave empty — submit still validated server-side */
				});
		}
	});

	async function submit(e: SubmitEvent) {
		e.preventDefault();
		if (submitting) return;
		const coord = coordinate.trim();
		if (!coord.includes('/')) {
			error = 'Coordinate must be vendor/slug (e.g. acme/mesh-prep).';
			return;
		}
		if (!category) {
			error = 'Pick a category.';
			return;
		}
		submitting = true;
		error = null;
		try {
			const updated = await promoteTemplate(template.id, {
				coordinate: coord,
				presentation: {
					vendor: vendor.trim() || undefined,
					category,
					icon: icon || undefined,
					color: color || undefined,
					badge: badge.trim() || undefined
				}
			});
			onpromoted?.(updated);
			open = false;
		} catch (err) {
			submitting = false;
			error =
				err instanceof ApiError
					? (err.body?.error ?? 'Could not promote template.')
					: 'Could not promote template.';
		}
	}
</script>

<Dialog bind:open>
	<DialogContent class="sm:max-w-md">
		<DialogHeader>
			<DialogTitle class="flex items-center gap-2">
				<Package class="size-4" />
				{isManage ? 'Manage library node' : 'Promote to library node'}
			</DialogTitle>
			<DialogDescription>
				A library node is a published template, branded and droppable from the
				editor's Library palette as a reusable building block.
			</DialogDescription>
		</DialogHeader>

		<form onsubmit={submit} class="space-y-4" data-testid="promote-form">
			<div class="space-y-1.5">
				<Label for="lib-coordinate">Coordinate</Label>
				<Input
					id="lib-coordinate"
					bind:value={coordinate}
					placeholder="acme/mesh-prep"
					autocomplete="off"
					class="font-mono"
					data-testid="promote-coordinate"
				/>
				<p class="text-xs text-muted-foreground">
					Stable <span class="font-mono">vendor/slug</span> handle. Lowercase letters,
					digits, hyphens.
				</p>
			</div>

			<div class="grid grid-cols-2 gap-3">
				<div class="space-y-1.5">
					<Label for="lib-vendor">Vendor</Label>
					<Input
						id="lib-vendor"
						bind:value={vendor}
						placeholder="Acme"
						autocomplete="off"
						data-testid="promote-vendor"
					/>
				</div>
				<div class="space-y-1.5">
					<Label>Category</Label>
					<Select.Root
						type="single"
						value={category}
						onValueChange={(v) => v && (category = v)}
					>
						<Select.Trigger class="w-full" data-testid="promote-category">
							<span class="truncate">{category || 'Select…'}</span>
						</Select.Trigger>
						<Select.Content>
							{#each categories as c (c)}
								<Select.Item value={c} label={c} />
							{/each}
						</Select.Content>
					</Select.Root>
				</div>
			</div>

			<div class="grid grid-cols-2 gap-3">
				<div class="space-y-1.5">
					<Label>Icon</Label>
					<Select.Root type="single" value={icon} onValueChange={(v) => v && (icon = v)}>
						<Select.Trigger class="w-full" data-testid="promote-icon">
							<span class="flex items-center gap-2 truncate" style="color: {color}">
								<PreviewIcon class="size-4" />
								<span class="text-foreground">{icon || 'default'}</span>
							</span>
						</Select.Trigger>
						<Select.Content>
							{#each iconKeys as k (k)}
								<Select.Item value={k} label={k} />
							{/each}
						</Select.Content>
					</Select.Root>
				</div>
				<div class="space-y-1.5">
					<Label for="lib-color">Accent</Label>
					<div class="flex items-center gap-2">
						<input
							id="lib-color"
							type="color"
							bind:value={color}
							class="size-9 shrink-0 cursor-pointer rounded-md border border-input bg-background"
							data-testid="promote-color"
						/>
						<Input bind:value={color} class="font-mono" autocomplete="off" />
					</div>
				</div>
			</div>

			<div class="space-y-1.5">
				<Label for="lib-badge">Badge <span class="text-muted-foreground">(optional)</span></Label>
				<Input
					id="lib-badge"
					bind:value={badge}
					placeholder="e.g. v2406"
					autocomplete="off"
					data-testid="promote-badge"
				/>
			</div>

			{#if error}
				<p class="text-sm text-destructive" data-testid="promote-error">{error}</p>
			{/if}

			<DialogFooter>
				<Button type="button" variant="ghost" onclick={() => (open = false)} disabled={submitting}>
					Cancel
				</Button>
				<Button type="submit" disabled={submitting} data-testid="promote-submit">
					{submitting ? 'Saving…' : isManage ? 'Update' : 'Promote'}
				</Button>
			</DialogFooter>
		</form>
	</DialogContent>
</Dialog>
