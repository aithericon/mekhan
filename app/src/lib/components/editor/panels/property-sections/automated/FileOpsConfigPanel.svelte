<script lang="ts">
	import KeyValueEditor from '../../shared/KeyValueEditor.svelte';
	import ResourcePicker from '../shared/ResourcePicker.svelte';
	import * as Select from '$lib/components/ui/select';
	import { Input } from '$lib/components/ui/input';
	import { Checkbox } from '$lib/components/ui/checkbox';
	import { FormField } from '$lib/components/ui/form-field';

	// Maps the storage backend to the workspace resource type that can
	// supply credentials + endpoint. Only `s3` has a workspace resource
	// today; gcs / azblob fall back to inline credentials until those
	// resource types ship.
	const resourceTypeForBackend: Record<string, string | null> = {
		local: null,
		s3: 's3',
		gcs: null,
		azblob: null
	};

	type Props = {
		config: Record<string, unknown>;
		readonly?: boolean;
		onchange: (config: Record<string, unknown>) => void;
	};

	let { config, readonly = false, onchange }: Props = $props();

	const operation = $derived((config.operation as string) ?? 'stat');
	const storage = $derived((config.storage as Record<string, unknown>) ?? {});
	const srcStorage = $derived(
		(config.source_storage as Record<string, unknown>) ?? {}
	);
	const dstStorage = $derived(
		(config.destination_storage as Record<string, unknown>) ?? null
	);

	function updateStorage(updates: Record<string, unknown>) {
		onchange({ ...config, storage: { ...storage, ...updates } });
	}

	function updateSrcStorage(updates: Record<string, unknown>) {
		onchange({ ...config, source_storage: { ...srcStorage, ...updates } });
	}

	const useSingleStorage = $derived(
		['stat', 'probe', 'delete', 'annotate', 'list'].includes(operation)
	);

	const operationLabels: Record<string, string> = {
		stat: 'Stat',
		probe: 'Probe',
		copy: 'Copy',
		move: 'Move',
		delete: 'Delete',
		list: 'List',
		annotate: 'Annotate'
	};

	const storageLabels: Record<string, string> = {
		local: 'Local',
		s3: 'S3',
		gcs: 'GCS',
		azblob: 'Azure Blob'
	};
</script>

<div class="space-y-1.5">
	<span class="text-sm font-medium text-muted-foreground">Operation</span>
	<Select.Root
		type="single"
		value={operation}
		onValueChange={(v) => {
			if (!v) return;
			const base: Record<string, unknown> = { operation: v };
			if (['stat', 'probe', 'delete'].includes(v)) {
				base.path = '';
				base.storage = storage;
			} else if (['copy', 'move'].includes(v)) {
				base.source = '';
				base.destination = '';
				base.source_storage = srcStorage;
			} else if (v === 'list') {
				base.prefix = '';
				base.storage = storage;
			} else if (v === 'annotate') {
				base.path = '';
				base.annotations = {};
				base.storage = storage;
			}
			onchange(base);
		}}
		disabled={readonly}
	>
		<Select.Trigger disabled={readonly}>
			{operationLabels[operation] ?? operation}
		</Select.Trigger>
		<Select.Content>
			<Select.Item value="stat" label="Stat" />
			<Select.Item value="probe" label="Probe" />
			<Select.Item value="copy" label="Copy" />
			<Select.Item value="move" label="Move" />
			<Select.Item value="delete" label="Delete" />
			<Select.Item value="list" label="List" />
			<Select.Item value="annotate" label="Annotate" />
		</Select.Content>
	</Select.Root>
</div>

<!-- Path fields per operation -->
{#if operation === 'list'}
	<FormField label="Prefix" for="fo-prefix">
		<Input
			id="fo-prefix"
			type="text"
			value={(config.prefix as string) ?? ''}
			placeholder="datasets/"
			disabled={readonly}
			oninput={(e) =>
				onchange({ ...config, prefix: (e.currentTarget as HTMLInputElement).value })}
			class="font-mono"
		/>
	</FormField>
	<FormField label="Limit (optional)" for="fo-limit">
		<Input
			id="fo-limit"
			type="number"
			min={1}
			value={(config.limit as number) ?? ''}
			placeholder="No limit"
			disabled={readonly}
			oninput={(e) => {
				const val = parseInt((e.currentTarget as HTMLInputElement).value);
				onchange({ ...config, limit: isNaN(val) ? undefined : val });
			}}
		/>
	</FormField>
{:else if operation === 'copy' || operation === 'move'}
	<FormField label="Source Path" for="fo-source">
		<Input
			id="fo-source"
			type="text"
			value={(config.source as string) ?? ''}
			placeholder="raw/data.csv"
			disabled={readonly}
			oninput={(e) =>
				onchange({ ...config, source: (e.currentTarget as HTMLInputElement).value })}
			class="font-mono"
		/>
	</FormField>
	<FormField label="Destination Path" for="fo-dest">
		<Input
			id="fo-dest"
			type="text"
			value={(config.destination as string) ?? ''}
			placeholder="processed/data.csv"
			disabled={readonly}
			oninput={(e) =>
				onchange({ ...config, destination: (e.currentTarget as HTMLInputElement).value })}
			class="font-mono"
		/>
	</FormField>
{:else if operation === 'annotate'}
	<FormField label="Path" for="fo-path">
		<Input
			id="fo-path"
			type="text"
			value={(config.path as string) ?? ''}
			placeholder="datasets/train.parquet"
			disabled={readonly}
			oninput={(e) =>
				onchange({ ...config, path: (e.currentTarget as HTMLInputElement).value })}
			class="font-mono"
		/>
	</FormField>
	<div class="space-y-1.5">
		<span class="text-sm font-medium text-muted-foreground">Annotations</span>
		<KeyValueEditor
			entries={(config.annotations as Record<string, unknown>) ?? {}}
			{readonly}
			onchange={(annotations) => onchange({ ...config, annotations })}
		/>
	</div>
	<label class="flex items-center gap-1.5 text-sm text-muted-foreground">
		<Checkbox
			checked={(config.merge as boolean) ?? false}
			disabled={readonly}
			onCheckedChange={(v) => onchange({ ...config, merge: v })}
		/>
		Merge with existing annotations
	</label>
{:else}
	<!-- stat, probe, delete -->
	<FormField label="Path" for="fo-path2">
		<Input
			id="fo-path2"
			type="text"
			value={(config.path as string) ?? ''}
			placeholder="datasets/train.parquet"
			disabled={readonly}
			oninput={(e) =>
				onchange({ ...config, path: (e.currentTarget as HTMLInputElement).value })}
			class="font-mono"
		/>
	</FormField>
	{#if operation === 'delete'}
		<label class="flex items-center gap-1.5 text-sm text-muted-foreground">
			<Checkbox
				checked={(config.ignore_missing as boolean) ?? false}
				disabled={readonly}
				onCheckedChange={(v) => onchange({ ...config, ignore_missing: v })}
			/>
			Ignore if missing
		</label>
	{/if}
{/if}

<!-- Storage config -->
{#if useSingleStorage}
	<div class="space-y-1.5 rounded-lg border border-border bg-muted/30 p-2">
		<span class="text-sm font-medium text-muted-foreground">Storage</span>
		<Select.Root
			type="single"
			value={(storage.backend as string) ?? 'local'}
			onValueChange={(v) => { if (v) updateStorage({ backend: v }); }}
			disabled={readonly}
		>
			<Select.Trigger disabled={readonly} class="h-6 px-1.5 py-0.5 text-sm">
				{storageLabels[(storage.backend as string) ?? 'local'] ?? 'Local'}
			</Select.Trigger>
			<Select.Content>
				<Select.Item value="local" label="Local" />
				<Select.Item value="s3" label="S3" />
				<Select.Item value="gcs" label="GCS" />
				<Select.Item value="azblob" label="Azure Blob" />
			</Select.Content>
		</Select.Root>
		<ResourcePicker
			resourceType={resourceTypeForBackend[(storage.backend as string) ?? 'local']}
			selected={(storage.resource_alias as string | undefined) ?? ''}
			onChange={(v) => updateStorage({ resource_alias: v || undefined })}
			label="Storage resource"
			{readonly}
			testId="file-ops-storage-resource"
			typeLabel={storageLabels[(storage.backend as string) ?? 'local']}
		/>
		<Input
			type="text"
			value={(storage.endpoint as string) ?? ''}
			placeholder={
				storage.resource_alias
					? 'Inherits from resource'
					: (storage.backend as string) === 'local'
						? '/tmp/store'
						: 'https://...'
			}
			disabled={readonly}
			oninput={(e) => updateStorage({ endpoint: (e.currentTarget as HTMLInputElement).value })}
			class="h-6 px-1.5 py-0.5 font-mono text-sm"
		/>
		{#if (storage.backend as string) !== 'local'}
			<Input
				type="text"
				value={(storage.bucket as string) ?? ''}
				placeholder={storage.resource_alias ? 'Inherits from resource' : 'Bucket name'}
				disabled={readonly}
				oninput={(e) => updateStorage({ bucket: (e.currentTarget as HTMLInputElement).value })}
				class="h-6 px-1.5 py-0.5 text-sm"
			/>
		{/if}
	</div>
{:else}
	<div class="space-y-1.5 rounded-lg border border-border bg-muted/30 p-2">
		<span class="text-sm font-medium text-muted-foreground">Source Storage</span>
		<Select.Root
			type="single"
			value={(srcStorage.backend as string) ?? 'local'}
			onValueChange={(v) => { if (v) updateSrcStorage({ backend: v }); }}
			disabled={readonly}
		>
			<Select.Trigger disabled={readonly} class="h-6 px-1.5 py-0.5 text-sm">
				{storageLabels[(srcStorage.backend as string) ?? 'local'] ?? 'Local'}
			</Select.Trigger>
			<Select.Content>
				<Select.Item value="local" label="Local" />
				<Select.Item value="s3" label="S3" />
				<Select.Item value="gcs" label="GCS" />
				<Select.Item value="azblob" label="Azure Blob" />
			</Select.Content>
		</Select.Root>
		<ResourcePicker
			resourceType={resourceTypeForBackend[(srcStorage.backend as string) ?? 'local']}
			selected={(srcStorage.resource_alias as string | undefined) ?? ''}
			onChange={(v) => updateSrcStorage({ resource_alias: v || undefined })}
			label="Source storage resource"
			{readonly}
			testId="file-ops-source-resource"
			typeLabel={storageLabels[(srcStorage.backend as string) ?? 'local']}
		/>
		<Input
			type="text"
			value={(srcStorage.endpoint as string) ?? ''}
			placeholder={
				srcStorage.resource_alias
					? 'Inherits from resource'
					: (srcStorage.backend as string) === 'local'
						? '/tmp/store'
						: 'https://...'
			}
			disabled={readonly}
			oninput={(e) => updateSrcStorage({ endpoint: (e.currentTarget as HTMLInputElement).value })}
			class="h-6 px-1.5 py-0.5 font-mono text-sm"
		/>
		{#if (srcStorage.backend as string) !== 'local'}
			<Input
				type="text"
				value={(srcStorage.bucket as string) ?? ''}
				placeholder={srcStorage.resource_alias ? 'Inherits from resource' : 'Bucket name'}
				disabled={readonly}
				oninput={(e) => updateSrcStorage({ bucket: (e.currentTarget as HTMLInputElement).value })}
				class="h-6 px-1.5 py-0.5 text-sm"
			/>
		{/if}
	</div>
{/if}
