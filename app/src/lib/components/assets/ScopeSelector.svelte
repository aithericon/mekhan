<script lang="ts">
	// Scope-context selector for the asset layer (docs/20 §2). Lets the author
	// pick the resolution context: the workspace, one of its projects, or a
	// specific template. The emitted `ScopeContext` drives downward-visibility
	// resolution on the list endpoints and the owner-scope on create.
	//
	// There is no single reusable "scope picker" in the codebase — this builds
	// a flat workspace ▸ folder cascade from the workspace store + listFolders.
	// (Template-level scoping is reachable from the editor, where the template id
	// is in context; the standalone /assets page exposes workspace + folder.)
	//
	// NOTE: the asset-layer `ScopeContext` still uses `kind: 'project'` (the
	// backend `scope_kind` for assets is independent of the template-folder
	// rename). We surface folders as the selectable grouping but keep the
	// existing scope token shape until the asset scope is rationalized.
	import { onMount } from 'svelte';
	import * as Select from '$lib/components/ui/select';
	import { auth } from '$lib/auth/store.svelte';
	import { listFolders, type Folder } from '$lib/api/client';
	import type { ScopeContext } from '$lib/api/assets';

	type Props = {
		value: ScopeContext;
		onChange: (scope: ScopeContext) => void;
		readonly?: boolean;
	};

	let { value, onChange, readonly = false }: Props = $props();

	let folders = $state<Folder[]>([]);

	const workspaceId = $derived(auth.session?.user.workspaceId ?? '');

	onMount(async () => {
		if (!workspaceId) return;
		try {
			folders = await listFolders(workspaceId);
		} catch {
			folders = [];
		}
	});

	// Encode the current ScopeContext into a flat select token.
	const selected = $derived.by(() => {
		if (value.kind === 'workspace') return 'workspace';
		return `${value.kind}:${value.id}`;
	});

	function onSelect(token: string | undefined) {
		if (!token || token === 'workspace') {
			onChange({ kind: 'workspace' });
			return;
		}
		const [kind, id] = token.split(':');
		if (kind === 'project' && id) onChange({ kind: 'project', id });
		else if (kind === 'template' && id) onChange({ kind: 'template', id });
		else onChange({ kind: 'workspace' });
	}

	const selectedLabel = $derived.by(() => {
		if (value.kind === 'workspace') return 'Workspace';
		if (value.kind === 'project') {
			const f = folders.find((f) => f.id === value.id);
			return `Folder: ${f?.display_name ?? value.id}`;
		}
		return `Template: ${value.id}`;
	});
</script>

<div class="flex items-center gap-2">
	<span class="text-sm font-medium text-muted-foreground">Scope</span>
	<Select.Root type="single" value={selected} onValueChange={onSelect} disabled={readonly}>
		<Select.Trigger class="h-9 min-w-[200px]" data-testid="asset-scope-selector">
			<span class="truncate text-sm">{selectedLabel}</span>
		</Select.Trigger>
		<Select.Content>
			<Select.Item value="workspace" label="Workspace" />
			{#each folders as f (f.id)}
				<Select.Item value={`project:${f.id}`} label={`Folder: ${f.display_name}`} />
			{/each}
		</Select.Content>
	</Select.Root>
</div>
