<script lang="ts">
	// Job-template management table for a single cluster.
	//
	// Lists job templates filtered to this cluster's flavor (all workspace-visible
	// templates when flavor is unknown). Provides create / edit / delete in an
	// inline expandable form, reusing existing Input/Textarea/Badge/Button widgets.

	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { Textarea } from '$lib/components/ui/textarea';
	import { FormField } from '$lib/components/ui/form-field';
	import { Checkbox } from '$lib/components/ui/checkbox';
	import Plus from '@lucide/svelte/icons/plus';
	import Pencil from '@lucide/svelte/icons/pencil';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import ChevronUp from '@lucide/svelte/icons/chevron-up';
	import {
		listJobTemplates,
		createJobTemplate,
		updateJobTemplate,
		deleteJobTemplate,
		type JobTemplateSummary,
		type CreateJobTemplateRequest,
		type UpdateJobTemplateRequest,
		type CommonSpec
	} from '$lib/api/job-templates';

	type Props = {
		/** Cluster flavor (`slurm` | `nomad`). Null when unknown — shows all templates. */
		flavor?: string | null;
		/** The datacenter resource id for this cluster (for display). */
		clusterId?: string;
	};

	let { flavor = null, clusterId }: Props = $props();

	let templates = $state<JobTemplateSummary[]>([]);
	let loading = $state(false);
	let error = $state<string | null>(null);

	async function load() {
		loading = true;
		error = null;
		try {
			const page = await listJobTemplates({ flavor: flavor ?? undefined, perPage: 200 });
			templates = page.items;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load job templates';
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		void flavor;
		void clusterId;
		load();
	});

	// ── Create / Edit form ────────────────────────────────────────────────────

	type FormMode = 'hidden' | 'create' | { editing: string };

	let formMode = $state<FormMode>('hidden');
	let formBusy = $state(false);
	let formError = $state<string | null>(null);

	// Form fields
	let fSlug = $state('');
	let fDisplayName = $state('');
	let fFlavor = $state('');
	let fImage = $state('');
	let fCpus = $state('');
	let fGpus = $state('');
	let fGpuType = $state('');
	let fMemMb = $state('');
	let fTimeLimit = $state('');
	let fPartition = $state('');
	let fEntrypoint = $state('');
	let fConsumerLocked = $state(false);
	let fEscapeHatch = $state('');

	function resetForm() {
		fSlug = '';
		fDisplayName = '';
		fFlavor = flavor ?? 'slurm';
		fImage = '';
		fCpus = '';
		fGpus = '';
		fGpuType = '';
		fMemMb = '';
		fTimeLimit = '';
		fPartition = '';
		fEntrypoint = '';
		fConsumerLocked = false;
		fEscapeHatch = '';
		formError = null;
	}

	function openCreate() {
		resetForm();
		formMode = 'create';
	}

	function openEdit(t: JobTemplateSummary) {
		resetForm();
		fSlug = t.slug;
		fDisplayName = t.display_name;
		fFlavor = t.flavor;
		fConsumerLocked = t.consumer_locked;
		// NOTE: CommonSpec fields are not available on the summary — the user
		// can edit them; they load blank (update merges, not replaces, optional fields).
		formMode = { editing: t.id };
	}

	function closeForm() {
		formMode = 'hidden';
		formError = null;
	}

	function buildCommonSpec(): CommonSpec {
		const spec: CommonSpec = {};
		if (fImage) spec.image = fImage;
		if (fEntrypoint) spec.entrypoint = fEntrypoint;
		const cpus = parseInt(fCpus, 10);
		if (!Number.isNaN(cpus) && fCpus !== '') spec.cpus = cpus;
		const gpus = parseInt(fGpus, 10);
		if (!Number.isNaN(gpus) && fGpus !== '') spec.gpus = gpus;
		if (fGpuType) spec.gpu_type = fGpuType;
		const mem = parseInt(fMemMb, 10);
		if (!Number.isNaN(mem) && fMemMb !== '') spec.mem_mb = mem;
		if (fTimeLimit) spec.time_limit = fTimeLimit;
		if (fPartition) spec.partition = fPartition;
		return spec;
	}

	function buildEscapeHatch() {
		const trimmed = fEscapeHatch.trim();
		if (!trimmed) return undefined;
		// Detect flavor: slurm escape hatch = sbatch_directives (lines starting with #SBATCH),
		// nomad = hcl_stanza (anything else).
		if (fFlavor === 'slurm') {
			return { sbatch_directives: trimmed.split('\n').filter(Boolean) };
		}
		return { hcl_stanza: trimmed };
	}

	async function submitCreate() {
		formBusy = true;
		formError = null;
		try {
			const body: CreateJobTemplateRequest = {
				slug: fSlug.trim(),
				display_name: fDisplayName.trim(),
				flavor: fFlavor,
				common_spec: buildCommonSpec(),
				consumer_locked: fConsumerLocked,
				escape_hatch: buildEscapeHatch()
			};
			await createJobTemplate(body);
			closeForm();
			await load();
		} catch (e) {
			formError = e instanceof Error ? e.message : 'Create failed';
		} finally {
			formBusy = false;
		}
	}

	async function submitEdit(id: string) {
		formBusy = true;
		formError = null;
		try {
			const body: UpdateJobTemplateRequest = {
				display_name: fDisplayName.trim() || undefined,
				common_spec: buildCommonSpec(),
				consumer_locked: fConsumerLocked,
				escape_hatch: buildEscapeHatch()
			};
			await updateJobTemplate(id, body);
			closeForm();
			await load();
		} catch (e) {
			formError = e instanceof Error ? e.message : 'Update failed';
		} finally {
			formBusy = false;
		}
	}

	// ── Delete ────────────────────────────────────────────────────────────────

	let deletingId = $state<string | null>(null);

	async function doDelete(id: string) {
		if (!confirm('Delete this job template? This is a soft delete — version history is preserved.')) return;
		deletingId = id;
		try {
			await deleteJobTemplate(id);
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Delete failed';
		} finally {
			deletingId = null;
		}
	}

	function flavorClass(f: string): string {
		if (f === 'slurm') return 'bg-sky-500/15 text-sky-700 dark:text-sky-300';
		if (f === 'nomad') return 'bg-emerald-500/15 text-emerald-700 dark:text-emerald-300';
		return 'bg-muted text-muted-foreground';
	}

	const isCreate = $derived(formMode === 'create');
	const editingId = $derived(typeof formMode === 'object' ? formMode.editing : null);
	const formOpen = $derived(formMode !== 'hidden');
</script>

<div class="space-y-4">
	<div class="flex items-center justify-between">
		<h2 class="text-base font-medium">Job templates</h2>
		<Button variant="outline" size="sm" onclick={openCreate} disabled={formOpen}>
			<Plus class="mr-1.5 size-4" />
			New template
		</Button>
	</div>

	{#if error}
		<div class="rounded-md border border-destructive/40 bg-destructive/5 px-3 py-2 text-sm text-destructive">
			{error}
		</div>
	{/if}

	<!-- Create / Edit form -->
	{#if formOpen}
		<div class="rounded-lg border border-border bg-muted/20 p-4 space-y-3">
			<div class="flex items-center justify-between">
				<span class="text-sm font-medium">{isCreate ? 'New job template' : 'Edit job template'}</span>
				<button
					type="button"
					class="text-sm text-muted-foreground hover:text-foreground"
					onclick={closeForm}
					aria-label="Close form"
				>
					{#if formOpen}
						<ChevronUp class="size-4" />
					{:else}
						<ChevronDown class="size-4" />
					{/if}
				</button>
			</div>

			<div class="grid grid-cols-2 gap-3">
				{#if isCreate}
					<FormField label="Slug (identifier key)" for="tpl-slug">
						<Input
							id="tpl-slug"
							type="text"
							class="font-mono text-sm"
							value={fSlug}
							placeholder="petri-mumax3-worker"
							oninput={(e) => (fSlug = (e.currentTarget as HTMLInputElement).value)}
						/>
					</FormField>
				{/if}
				<FormField label="Display name" for="tpl-display-name">
					<Input
						id="tpl-display-name"
						type="text"
						class="text-sm"
						value={fDisplayName}
						placeholder="mumax3 (micromagnetics, GPU)"
						oninput={(e) => (fDisplayName = (e.currentTarget as HTMLInputElement).value)}
					/>
				</FormField>
				{#if isCreate}
					<FormField label="Flavor" for="tpl-flavor">
						<Input
							id="tpl-flavor"
							type="text"
							class="font-mono text-sm"
							value={fFlavor}
							placeholder="slurm"
							oninput={(e) => (fFlavor = (e.currentTarget as HTMLInputElement).value)}
						/>
					</FormField>
				{/if}
			</div>

			<p class="text-sm font-medium text-muted-foreground pt-1">Common spec</p>
			<div class="grid grid-cols-2 gap-3">
				<FormField label="Image" for="tpl-image">
					<Input
						id="tpl-image"
						type="text"
						class="font-mono text-sm"
						value={fImage}
						placeholder="registry.example.com/mumax3:latest"
						oninput={(e) => (fImage = (e.currentTarget as HTMLInputElement).value)}
					/>
				</FormField>
				<FormField label="Entrypoint" for="tpl-entrypoint">
					<Input
						id="tpl-entrypoint"
						type="text"
						class="font-mono text-sm"
						value={fEntrypoint}
						placeholder="run.sh"
						oninput={(e) => (fEntrypoint = (e.currentTarget as HTMLInputElement).value)}
					/>
				</FormField>
				<FormField label="CPUs" for="tpl-cpus">
					<Input
						id="tpl-cpus"
						type="number"
						class="text-sm"
						value={fCpus}
						placeholder="4"
						oninput={(e) => (fCpus = (e.currentTarget as HTMLInputElement).value)}
					/>
				</FormField>
				<FormField label="GPUs" for="tpl-gpus">
					<Input
						id="tpl-gpus"
						type="number"
						class="text-sm"
						value={fGpus}
						placeholder="1"
						oninput={(e) => (fGpus = (e.currentTarget as HTMLInputElement).value)}
					/>
				</FormField>
				<FormField label="GPU type" for="tpl-gpu-type">
					<Input
						id="tpl-gpu-type"
						type="text"
						class="font-mono text-sm"
						value={fGpuType}
						placeholder="A100"
						oninput={(e) => (fGpuType = (e.currentTarget as HTMLInputElement).value)}
					/>
				</FormField>
				<FormField label="Memory (MB)" for="tpl-mem">
					<Input
						id="tpl-mem"
						type="number"
						class="text-sm"
						value={fMemMb}
						placeholder="8192"
						oninput={(e) => (fMemMb = (e.currentTarget as HTMLInputElement).value)}
					/>
				</FormField>
				<FormField label="Time limit" for="tpl-time">
					<Input
						id="tpl-time"
						type="text"
						class="font-mono text-sm"
						value={fTimeLimit}
						placeholder="01:30:00"
						oninput={(e) => (fTimeLimit = (e.currentTarget as HTMLInputElement).value)}
					/>
				</FormField>
				<FormField label="Partition" for="tpl-partition">
					<Input
						id="tpl-partition"
						type="text"
						class="font-mono text-sm"
						value={fPartition}
						placeholder="gpu"
						oninput={(e) => (fPartition = (e.currentTarget as HTMLInputElement).value)}
					/>
				</FormField>
			</div>

			<FormField
				label={fFlavor === 'slurm'
					? 'Escape hatch (#SBATCH directives, one per line)'
					: 'Escape hatch (HCL job stanza)'}
				for="tpl-escape-hatch"
			>
				<Textarea
					id="tpl-escape-hatch"
					class="font-mono text-sm"
					rows={3}
					value={fEscapeHatch}
					placeholder={fFlavor === 'slurm'
						? '#SBATCH --exclusive\n#SBATCH --constraint=gpu_a100'
						: '# raw HCL stanza fields'}
					oninput={(e) => (fEscapeHatch = (e.currentTarget as HTMLTextAreaElement).value)}
				/>
			</FormField>

			<label class="flex items-center gap-1.5 text-sm text-muted-foreground">
				<Checkbox
					checked={fConsumerLocked}
					onCheckedChange={(v) => (fConsumerLocked = v === true)}
				/>
				Consumer locked (restrict to workspace members)
			</label>

			{#if formError}
				<p class="text-sm text-destructive">{formError}</p>
			{/if}

			<div class="flex items-center gap-2 pt-1">
				<Button
					size="sm"
					disabled={formBusy}
					onclick={() => {
						if (isCreate) submitCreate();
						else if (editingId) submitEdit(editingId);
					}}
				>
					{formBusy ? 'Saving…' : isCreate ? 'Create' : 'Save changes'}
				</Button>
				<Button variant="ghost" size="sm" onclick={closeForm} disabled={formBusy}>
					Cancel
				</Button>
			</div>
		</div>
	{/if}

	<!-- Table -->
	{#if loading && templates.length === 0}
		<p class="text-sm text-muted-foreground">Loading…</p>
	{:else if templates.length === 0}
		<div class="rounded-lg border border-dashed border-border/60 px-4 py-8 text-center">
			<p class="text-sm text-muted-foreground">
				No job templates yet for this cluster.
				<button
					type="button"
					class="underline underline-offset-2 hover:text-foreground"
					onclick={openCreate}
				>
					Create the first one.
				</button>
			</p>
		</div>
	{:else}
		<div class="overflow-x-auto rounded-lg border border-border/60">
			<table class="w-full text-sm">
				<thead>
					<tr class="border-b border-border/40 bg-muted/30 text-left text-xs text-muted-foreground">
						<th class="px-3 py-2 font-medium">Slug</th>
						<th class="px-3 py-2 font-medium">Display name</th>
						<th class="px-3 py-2 font-medium">Flavor</th>
						<th class="px-3 py-2 font-medium">Version</th>
						<th class="px-3 py-2 font-medium">Visibility</th>
						<th class="px-3 py-2"></th>
					</tr>
				</thead>
				<tbody>
					{#each templates as t (t.id)}
						<tr class="border-b border-border/30 last:border-0 hover:bg-muted/10 transition-colors">
							<td class="px-3 py-2 font-mono text-sm">{t.slug}</td>
							<td class="px-3 py-2 text-sm">{t.display_name}</td>
							<td class="px-3 py-2">
								<Badge class={`text-xs ${flavorClass(t.flavor)}`}>{t.flavor}</Badge>
							</td>
							<td class="px-3 py-2 text-sm tabular-nums">v{t.latest_version}</td>
							<td class="px-3 py-2">
								<Badge variant="secondary" class="text-xs">{t.visibility}</Badge>
							</td>
							<td class="px-3 py-2">
								<div class="flex items-center justify-end gap-1">
									<Button
										variant="ghost"
										size="sm"
										class="h-7 w-7 p-0"
										disabled={typeof editingId === 'string' || formBusy}
										onclick={() => openEdit(t)}
										aria-label="Edit"
									>
										<Pencil class="size-3.5" />
									</Button>
									<Button
										variant="ghost"
										size="sm"
										class="h-7 w-7 p-0 text-destructive hover:text-destructive"
										disabled={deletingId === t.id}
										onclick={() => doDelete(t.id)}
										aria-label="Delete"
									>
										<Trash2 class="size-3.5" />
									</Button>
								</div>
							</td>
						</tr>
					{/each}
				</tbody>
			</table>
		</div>
	{/if}
</div>
