<script lang="ts">
	// Shared picker for binding an AutomatedStep to a workspace job-template.
	//
	// Mirrors ResourcePicker.svelte: async-loads job templates (filtered by
	// `flavor` when known), renders a Select with a ref value of
	// `{ template_id, version }`, and degrades gracefully to an empty-state hint.

	import { FormField } from '$lib/components/ui/form-field';
	import * as Select from '$lib/components/ui/select';
	import { listJobTemplates, type JobTemplateSummary } from '$lib/api/job-templates';

	/** A resolved template ref that the parent stores on the node. */
	export interface JobTemplateRef {
		template_id: string;
		/** `null` means "always use latest". Stored as null so future
		 *  pinned-version logic can be added without a field rename. */
		version: number | null;
	}

	type Props = {
		/** When provided, only templates of this flavor are shown. */
		flavor?: string | null;
		selected: JobTemplateRef | null;
		onChange: (ref: JobTemplateRef | null) => void;
		label?: string;
		readonly?: boolean;
		testId?: string;
	};

	let {
		flavor = null,
		selected,
		onChange,
		label = 'Job template',
		readonly = false,
		testId
	}: Props = $props();

	let templates = $state<JobTemplateSummary[]>([]);
	let loading = $state(false);
	let error = $state<string | null>(null);
	let lastLoadedFlavor: string | null | undefined = undefined;

	async function load(f: string | null) {
		loading = true;
		error = null;
		try {
			const page = await listJobTemplates({ flavor: f ?? undefined, perPage: 200 });
			templates = page.items;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load job templates';
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		if (flavor !== lastLoadedFlavor) {
			lastLoadedFlavor = flavor;
			load(flavor);
		}
	});

	/** Encode the ref as a stable string for the Select value. */
	function encode(ref: JobTemplateRef | null): string {
		if (!ref) return '';
		return ref.template_id;
	}

	function selectedLabel(): string {
		if (!selected) return loading ? 'Loading…' : 'None — use env-global template';
		const found = templates.find((t) => t.id === selected.template_id);
		if (found) {
			return `${found.slug} — ${found.display_name} (v${found.latest_version})`;
		}
		return selected.template_id;
	}

	function handleChange(id: string | undefined) {
		if (!id) {
			onChange(null);
			return;
		}
		const found = templates.find((t) => t.id === id);
		if (found) {
			onChange({ template_id: found.id, version: null });
		}
	}
</script>

<div class="space-y-1.5">
	<FormField {label} for={testId ?? 'job-template-picker'}>
		<Select.Root
			type="single"
			value={encode(selected)}
			onValueChange={handleChange}
			disabled={readonly || loading}
		>
			<Select.Trigger disabled={readonly || loading} data-testid={testId}>
				<span class="truncate text-sm">{selectedLabel()}</span>
			</Select.Trigger>
			<Select.Content>
				<Select.Item value="" label="None — use env-global template" />
				{#each templates as t (t.id)}
					<Select.Item
						value={t.id}
						label={`${t.slug} — ${t.display_name} (v${t.latest_version})`}
					/>
				{/each}
			</Select.Content>
		</Select.Root>
	</FormField>
	{#if error}
		<p class="text-sm text-destructive">{error}</p>
	{:else if templates.length === 0 && !loading}
		<p class="text-sm italic text-muted-foreground">
			No job templates in this workspace. Create one under
			<code class="font-mono">Clusters &rarr; Templates</code>.
		</p>
	{:else if selected}
		<p class="text-sm italic text-muted-foreground">
			Always uses the latest published version of this template.
		</p>
	{/if}
</div>
