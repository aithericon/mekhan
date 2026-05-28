<script lang="ts">
	import Tag from '@lucide/svelte/icons/tag';
	import X from '@lucide/svelte/icons/x';
	import Globe from '@lucide/svelte/icons/globe';
	import Lock from '@lucide/svelte/icons/lock';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { Badge } from '$lib/components/ui/badge';
	import {
		getTemplateTags,
		setTemplateTags,
		setTemplateVisibility,
		type Template
	} from '$lib/api/client';

	interface Props {
		template: Template;
	}

	let { template }: Props = $props();

	type Visibility = 'workspace' | 'public';

	let tags = $state<string[]>([]);
	let draftTag = $state('');
	let dirty = $state(false);
	let loadingTags = $state(true);
	let savingTags = $state(false);
	let tagError = $state<string | null>(null);

	// Visibility tracks the template row by default; an optimistic edit sets
	// `override` so the toggle reflects the in-flight PATCH, and reverts on
	// failure. Deriving (rather than copying once) keeps it correct if the
	// template prop is reassigned (e.g. after a new-version fork).
	let visibilityOverride = $state<Visibility | null>(null);
	const visibility = $derived<Visibility>(
		visibilityOverride ?? (template.visibility as Visibility) ?? 'workspace'
	);
	let savingVisibility = $state(false);
	let visibilityError = $state<string | null>(null);

	$effect(() => {
		loadTags(template.id);
	});

	async function loadTags(id: string) {
		loadingTags = true;
		tagError = null;
		try {
			tags = await getTemplateTags(id);
			dirty = false;
		} catch (e) {
			tagError = e instanceof Error ? e.message : 'Failed to load tags';
			tags = [];
		} finally {
			loadingTags = false;
		}
	}

	function addTag() {
		const t = draftTag.trim();
		if (!t) return;
		if (!tags.includes(t)) {
			tags = [...tags, t];
			dirty = true;
		}
		draftTag = '';
	}

	function removeTag(t: string) {
		tags = tags.filter((x) => x !== t);
		dirty = true;
	}

	function onTagKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter' || e.key === ',') {
			e.preventDefault();
			addTag();
		}
	}

	async function saveTags() {
		savingTags = true;
		tagError = null;
		try {
			tags = await setTemplateTags(template.id, tags);
			dirty = false;
		} catch (e) {
			tagError = e instanceof Error ? e.message : 'Failed to save tags';
		} finally {
			savingTags = false;
		}
	}

	async function changeVisibility(next: Visibility) {
		if (next === visibility || savingVisibility) return;
		const prev = visibility;
		visibilityOverride = next; // optimistic
		savingVisibility = true;
		visibilityError = null;
		try {
			await setTemplateVisibility(template.id, next);
		} catch (e) {
			visibilityOverride = prev;
			visibilityError = e instanceof Error ? e.message : 'Failed to update visibility';
		} finally {
			savingVisibility = false;
		}
	}
</script>

<div class="flex h-full flex-col gap-8 overflow-y-auto p-6" data-testid="template-settings-panel">
	<header>
		<h2 class="text-lg font-semibold tracking-tight">Template settings</h2>
		<p class="text-sm text-muted-foreground">
			Tags and visibility apply to every version of this template.
		</p>
	</header>

	<!-- Tags -->
	<section class="space-y-3">
		<div class="flex items-center gap-2 text-sm font-medium">
			<Tag class="size-4 text-muted-foreground" />
			Tags
		</div>
		<p class="text-sm text-muted-foreground">
			Free-form labels used to filter the templates list. Editor role required to save.
		</p>

		{#if loadingTags}
			<div class="text-sm text-muted-foreground">Loading tags…</div>
		{:else}
			<div class="flex flex-wrap gap-1.5" data-testid="settings-tag-chips">
				{#each tags as t (t)}
					<Badge variant="secondary" class="gap-1 pr-1">
						{t}
						<button
							type="button"
							class="rounded hover:text-destructive"
							onclick={() => removeTag(t)}
							data-testid={`settings-remove-tag-${t}`}
							aria-label={`Remove tag ${t}`}
						>
							<X class="size-3" />
						</button>
					</Badge>
				{:else}
					<span class="text-sm text-muted-foreground/60 italic">No tags yet</span>
				{/each}
			</div>

			<div class="flex gap-2">
				<Input
					placeholder="Add a tag…"
					bind:value={draftTag}
					onkeydown={onTagKeydown}
					data-testid="settings-tag-input"
					class="flex-1"
				/>
				<Button
					variant="outline"
					size="sm"
					onclick={addTag}
					disabled={!draftTag.trim()}
					data-testid="settings-add-tag"
				>
					Add
				</Button>
			</div>

			<div class="flex items-center gap-3">
				<Button
					size="sm"
					onclick={saveTags}
					disabled={!dirty || savingTags}
					data-testid="settings-save-tags"
				>
					{savingTags ? 'Saving…' : 'Save tags'}
				</Button>
				{#if dirty && !savingTags}
					<span class="text-sm text-muted-foreground">Unsaved changes</span>
				{/if}
			</div>
			{#if tagError}
				<div class="text-sm text-destructive" data-testid="settings-tag-error">{tagError}</div>
			{/if}
		{/if}
	</section>

	<!-- Visibility -->
	<section class="space-y-3">
		<div class="flex items-center gap-2 text-sm font-medium">
			{#if visibility === 'public'}
				<Globe class="size-4 text-muted-foreground" />
			{:else}
				<Lock class="size-4 text-muted-foreground" />
			{/if}
			Visibility
		</div>
		<p class="text-sm text-muted-foreground">
			Public templates are readable across workspaces. Changing this is a tenancy
			decision — admin role required.
		</p>

		<div class="inline-flex rounded-md border border-border p-0.5" data-testid="settings-visibility">
			<button
				type="button"
				class="rounded px-3 py-1 text-sm transition-colors {visibility === 'workspace' ? 'bg-foreground text-background' : 'text-muted-foreground hover:bg-accent'}"
				onclick={() => changeVisibility('workspace')}
				disabled={savingVisibility}
				data-testid="settings-visibility-workspace"
			>
				Workspace
			</button>
			<button
				type="button"
				class="rounded px-3 py-1 text-sm transition-colors {visibility === 'public' ? 'bg-foreground text-background' : 'text-muted-foreground hover:bg-accent'}"
				onclick={() => changeVisibility('public')}
				disabled={savingVisibility}
				data-testid="settings-visibility-public"
			>
				Public
			</button>
		</div>
		{#if visibilityError}
			<div class="text-sm text-destructive" data-testid="settings-visibility-error">
				{visibilityError}
			</div>
		{/if}
	</section>
</div>
