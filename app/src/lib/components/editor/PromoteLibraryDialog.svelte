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
		setLifecycle,
		listLibraryCategories,
		uploadLibraryIcon,
		libraryIconUrl,
		isAssetIcon,
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

	// Lifecycle (Phase 5) — only meaningful once it's a library node.
	let successor = $state('');
	let lifecycleBusy = $state(false);

	// Custom-logo upload (the `icon` field may hold an `asset:{uuid}` token).
	let uploading = $state(false);
	let fileInput = $state<HTMLInputElement | null>(null);

	const isManage = $derived(template?.template_kind === 'library_node');
	const lifecycle = $derived(template?.lifecycle_status ?? 'active');
	const PreviewIcon = $derived(resolveNodeIcon(icon));
	// When the icon is an uploaded logo, render its served image; otherwise the
	// named-registry PreviewIcon path is used.
	const assetIcon = $derived(isAssetIcon(icon));
	const assetIconSrc = $derived(libraryIconUrl(icon));

	function pickLogo() {
		error = null;
		fileInput?.click();
	}

	async function onLogoSelected(e: Event) {
		const target = e.currentTarget as HTMLInputElement;
		const file = target.files?.[0];
		// Reset the input so re-selecting the same file fires `change` again.
		target.value = '';
		if (!file) return;
		uploading = true;
		error = null;
		try {
			const res = await uploadLibraryIcon(file);
			icon = res.icon;
		} catch (err) {
			error =
				err instanceof ApiError
					? (err.body?.error ?? 'Could not upload logo.')
					: 'Could not upload logo.';
		} finally {
			uploading = false;
		}
	}

	function clearLogo() {
		error = null;
		icon = '';
	}

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
		successor = template?.superseded_by ?? '';
		error = null;
		submitting = false;
		lifecycleBusy = false;
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

	async function changeLifecycle(status: 'active' | 'deprecated' | 'retired') {
		if (lifecycleBusy) return;
		const succ = successor.trim();
		if (status !== 'active' && succ && !succ.includes('/')) {
			error = 'Successor must be a vendor/slug coordinate.';
			return;
		}
		lifecycleBusy = true;
		error = null;
		try {
			const updated = await setLifecycle(template.id, {
				status,
				superseded_by: status === 'active' ? undefined : succ || undefined
			});
			onpromoted?.(updated);
		} catch (err) {
			error =
				err instanceof ApiError
					? (err.body?.error ?? 'Could not change lifecycle.')
					: 'Could not change lifecycle.';
		} finally {
			lifecycleBusy = false;
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
					{#if assetIcon}
						<div
							class="flex items-center gap-2 rounded-md border border-input bg-background px-2 py-1.5"
						>
							{#if assetIconSrc}
								<img
									src={assetIconSrc}
									alt="Custom logo"
									class="size-5 shrink-0 rounded-sm object-contain"
								/>
							{/if}
							<span class="truncate text-sm text-foreground">Custom logo</span>
							<Button
								type="button"
								variant="ghost"
								size="sm"
								class="ml-auto h-7 px-2 text-xs"
								onclick={clearLogo}
								data-testid="promote-clear-logo"
							>
								Use named icon
							</Button>
						</div>
					{:else}
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
						<Button
							type="button"
							variant="outline"
							size="sm"
							class="w-full"
							disabled={uploading}
							onclick={pickLogo}
							data-testid="promote-upload-logo"
						>
							{uploading ? 'Uploading…' : 'Upload logo'}
						</Button>
					{/if}
					<input
						bind:this={fileInput}
						type="file"
						accept="image/*"
						class="hidden"
						onchange={onLogoSelected}
						aria-hidden="true"
						tabindex="-1"
					/>
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

			{#if isManage}
				<div class="space-y-2 rounded-md border border-border/60 p-3" data-testid="lifecycle-section">
					<div class="flex items-center justify-between">
						<Label>Lifecycle</Label>
						<span
							class="rounded px-1.5 py-0.5 text-xs font-medium uppercase {lifecycle === 'active'
								? 'bg-emerald-500/15 text-emerald-600'
								: lifecycle === 'deprecated'
									? 'bg-amber-500/15 text-amber-600'
									: 'bg-muted text-muted-foreground'}"
							data-testid="lifecycle-status"
						>
							{lifecycle}
						</span>
					</div>
					<p class="text-xs text-muted-foreground">
						Deprecated nodes stay droppable with a warning; retired nodes are hidden
						from the palette. Existing embeds keep resolving either way.
					</p>
					<Input
						bind:value={successor}
						placeholder="successor coordinate (optional, vendor/slug)"
						autocomplete="off"
						class="font-mono"
						data-testid="lifecycle-successor"
					/>
					<div class="flex gap-2">
						{#if lifecycle !== 'active'}
							<Button
								type="button"
								variant="outline"
								size="sm"
								class="flex-1"
								disabled={lifecycleBusy}
								onclick={() => changeLifecycle('active')}
								data-testid="lifecycle-reactivate"
							>
								Reactivate
							</Button>
						{/if}
						{#if lifecycle !== 'deprecated'}
							<Button
								type="button"
								variant="outline"
								size="sm"
								class="flex-1"
								disabled={lifecycleBusy}
								onclick={() => changeLifecycle('deprecated')}
								data-testid="lifecycle-deprecate"
							>
								Deprecate
							</Button>
						{/if}
						{#if lifecycle !== 'retired'}
							<Button
								type="button"
								variant="outline"
								size="sm"
								class="flex-1"
								disabled={lifecycleBusy}
								onclick={() => changeLifecycle('retired')}
								data-testid="lifecycle-retire"
							>
								Retire
							</Button>
						{/if}
					</div>
				</div>
			{/if}

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
