<script lang="ts">
	import KeyValueEditor from '../../shared/KeyValueEditor.svelte';
	import { Select, SelectTrigger, SelectContent, SelectItem } from '$lib/components/ui/select';

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
	<span class="text-xs font-medium text-muted-foreground">Operation</span>
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
		<SelectTrigger disabled={readonly}>
			{operationLabels[operation] ?? operation}
		</SelectTrigger>
		<SelectContent>
			<SelectItem value="stat" label="Stat" />
			<SelectItem value="probe" label="Probe" />
			<SelectItem value="copy" label="Copy" />
			<SelectItem value="move" label="Move" />
			<SelectItem value="delete" label="Delete" />
			<SelectItem value="list" label="List" />
			<SelectItem value="annotate" label="Annotate" />
		</SelectContent>
	</Select.Root>
</div>

<!-- Path fields per operation -->
{#if operation === 'list'}
	<div class="space-y-1.5">
		<label for="fo-prefix" class="text-xs font-medium text-muted-foreground">Prefix</label>
		<input
			id="fo-prefix"
			type="text"
			value={(config.prefix as string) ?? ''}
			placeholder="datasets/"
			disabled={readonly}
			oninput={(e) =>
				onchange({ ...config, prefix: (e.currentTarget as HTMLInputElement).value })}
			class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 font-mono text-sm text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
		/>
	</div>
	<div class="space-y-1.5">
		<label for="fo-limit" class="text-xs font-medium text-muted-foreground"
			>Limit (optional)</label
		>
		<input
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
			class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-sm text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
		/>
	</div>
{:else if operation === 'copy' || operation === 'move'}
	<div class="space-y-1.5">
		<label for="fo-source" class="text-xs font-medium text-muted-foreground">Source Path</label>
		<input
			id="fo-source"
			type="text"
			value={(config.source as string) ?? ''}
			placeholder="raw/data.csv"
			disabled={readonly}
			oninput={(e) =>
				onchange({ ...config, source: (e.currentTarget as HTMLInputElement).value })}
			class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 font-mono text-sm text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
		/>
	</div>
	<div class="space-y-1.5">
		<label for="fo-dest" class="text-xs font-medium text-muted-foreground"
			>Destination Path</label
		>
		<input
			id="fo-dest"
			type="text"
			value={(config.destination as string) ?? ''}
			placeholder="processed/data.csv"
			disabled={readonly}
			oninput={(e) =>
				onchange({ ...config, destination: (e.currentTarget as HTMLInputElement).value })}
			class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 font-mono text-sm text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
		/>
	</div>
{:else if operation === 'annotate'}
	<div class="space-y-1.5">
		<label for="fo-path" class="text-xs font-medium text-muted-foreground">Path</label>
		<input
			id="fo-path"
			type="text"
			value={(config.path as string) ?? ''}
			placeholder="datasets/train.parquet"
			disabled={readonly}
			oninput={(e) =>
				onchange({ ...config, path: (e.currentTarget as HTMLInputElement).value })}
			class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 font-mono text-sm text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
		/>
	</div>
	<div class="space-y-1.5">
		<span class="text-xs font-medium text-muted-foreground">Annotations</span>
		<KeyValueEditor
			entries={(config.annotations as Record<string, unknown>) ?? {}}
			{readonly}
			onchange={(annotations) => onchange({ ...config, annotations })}
		/>
	</div>
	<label class="flex items-center gap-1.5 text-xs text-muted-foreground">
		<input
			type="checkbox"
			checked={(config.merge as boolean) ?? false}
			disabled={readonly}
			onchange={(e) =>
				onchange({ ...config, merge: (e.currentTarget as HTMLInputElement).checked })}
			class="size-3.5 disabled:cursor-default disabled:opacity-70"
		/>
		Merge with existing annotations
	</label>
{:else}
	<!-- stat, probe, delete -->
	<div class="space-y-1.5">
		<label for="fo-path2" class="text-xs font-medium text-muted-foreground">Path</label>
		<input
			id="fo-path2"
			type="text"
			value={(config.path as string) ?? ''}
			placeholder="datasets/train.parquet"
			disabled={readonly}
			oninput={(e) =>
				onchange({ ...config, path: (e.currentTarget as HTMLInputElement).value })}
			class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 font-mono text-sm text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
		/>
	</div>
	{#if operation === 'delete'}
		<label class="flex items-center gap-1.5 text-xs text-muted-foreground">
			<input
				type="checkbox"
				checked={(config.ignore_missing as boolean) ?? false}
				disabled={readonly}
				onchange={(e) =>
					onchange({ ...config, ignore_missing: (e.currentTarget as HTMLInputElement).checked })}
				class="size-3.5 disabled:cursor-default disabled:opacity-70"
			/>
			Ignore if missing
		</label>
	{/if}
{/if}

<!-- Storage config -->
{#if useSingleStorage}
	<div class="space-y-1.5 rounded-lg border border-border bg-muted/30 p-2">
		<span class="text-[10px] font-medium text-muted-foreground">Storage</span>
		<Select.Root
			type="single"
			value={(storage.backend as string) ?? 'local'}
			onValueChange={(v) => { if (v) updateStorage({ backend: v }); }}
			disabled={readonly}
		>
			<SelectTrigger disabled={readonly} class="h-6 px-1.5 py-0.5 text-[10px]">
				{storageLabels[(storage.backend as string) ?? 'local'] ?? 'Local'}
			</SelectTrigger>
			<SelectContent>
				<SelectItem value="local" label="Local" />
				<SelectItem value="s3" label="S3" />
				<SelectItem value="gcs" label="GCS" />
				<SelectItem value="azblob" label="Azure Blob" />
			</SelectContent>
		</Select.Root>
		<input
			type="text"
			value={(storage.endpoint as string) ?? ''}
			placeholder={(storage.backend as string) === 'local' ? '/tmp/store' : 'https://...'}
			disabled={readonly}
			oninput={(e) => updateStorage({ endpoint: (e.currentTarget as HTMLInputElement).value })}
			class="w-full rounded border border-input bg-background px-1.5 py-0.5 font-mono text-[10px] focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
		/>
		{#if (storage.backend as string) !== 'local'}
			<input
				type="text"
				value={(storage.bucket as string) ?? ''}
				placeholder="Bucket name"
				disabled={readonly}
				oninput={(e) => updateStorage({ bucket: (e.currentTarget as HTMLInputElement).value })}
				class="w-full rounded border border-input bg-background px-1.5 py-0.5 text-[10px] focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
			/>
		{/if}
	</div>
{:else}
	<div class="space-y-1.5 rounded-lg border border-border bg-muted/30 p-2">
		<span class="text-[10px] font-medium text-muted-foreground">Source Storage</span>
		<Select.Root
			type="single"
			value={(srcStorage.backend as string) ?? 'local'}
			onValueChange={(v) => { if (v) updateSrcStorage({ backend: v }); }}
			disabled={readonly}
		>
			<SelectTrigger disabled={readonly} class="h-6 px-1.5 py-0.5 text-[10px]">
				{storageLabels[(srcStorage.backend as string) ?? 'local'] ?? 'Local'}
			</SelectTrigger>
			<SelectContent>
				<SelectItem value="local" label="Local" />
				<SelectItem value="s3" label="S3" />
				<SelectItem value="gcs" label="GCS" />
				<SelectItem value="azblob" label="Azure Blob" />
			</SelectContent>
		</Select.Root>
		<input
			type="text"
			value={(srcStorage.endpoint as string) ?? ''}
			placeholder={(srcStorage.backend as string) === 'local' ? '/tmp/store' : 'https://...'}
			disabled={readonly}
			oninput={(e) => updateSrcStorage({ endpoint: (e.currentTarget as HTMLInputElement).value })}
			class="w-full rounded border border-input bg-background px-1.5 py-0.5 font-mono text-[10px] focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
		/>
		{#if (srcStorage.backend as string) !== 'local'}
			<input
				type="text"
				value={(srcStorage.bucket as string) ?? ''}
				placeholder="Bucket name"
				disabled={readonly}
				oninput={(e) => updateSrcStorage({ bucket: (e.currentTarget as HTMLInputElement).value })}
				class="w-full rounded border border-input bg-background px-1.5 py-0.5 text-[10px] focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
			/>
		{/if}
	</div>
{/if}
