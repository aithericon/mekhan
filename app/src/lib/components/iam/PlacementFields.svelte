<script lang="ts">
	// Shared create-time PLACEMENT controls for the object-ACL layer: where an
	// object lives (its scope = ACL inheritance parent) and whether it's private
	// (`restricted` drops the workspace-role floor). Resources and assets create
	// flows render this identically so the two surfaces read the same — the user
	// asked for create scoping to be one shared, consolidated component.
	//
	// `scope` and `restricted` are $bindable so callers two-way bind their own
	// create-form state. Selecting a folder scopes the object to that folder's
	// subtree and inherits its access; the workspace default is visible to
	// everyone at their workspace role (today's behaviour).
	import { FormField } from '$lib/components/ui/form-field';
	import Lock from '@lucide/svelte/icons/lock';
	import ScopeSelector from '$lib/components/assets/ScopeSelector.svelte';
	import type { ScopeContext } from '$lib/api/assets';

	let {
		scope = $bindable(),
		restricted = $bindable(),
		testidPrefix = 'placement'
	}: {
		scope: ScopeContext;
		restricted: boolean;
		/** data-testid stem for the Private toggle (e.g. `resource`, `asset`). */
		testidPrefix?: string;
	} = $props();
</script>

<FormField
	label="Location"
	description="Workspace = visible to everyone. A folder scopes it to that folder's subtree and inherits the folder's access."
>
	<ScopeSelector value={scope} onChange={(s) => (scope = s)} />
</FormField>

<label
	class="flex items-start gap-2.5 rounded-md border border-border/60 p-3 text-sm"
	data-testid="{testidPrefix}-restricted"
>
	<input
		type="checkbox"
		checked={restricted}
		onchange={(e) => (restricted = (e.currentTarget as HTMLInputElement).checked)}
		class="mt-0.5 size-4"
	/>
	<span>
		<span class="flex items-center gap-1.5 font-medium text-foreground">
			<Lock class="size-3.5" /> Private
		</span>
		<span class="text-muted-foreground">
			Not shared workspace-wide. Only you and people explicitly granted access
			(plus workspace admins) can see it.
		</span>
	</span>
</label>
