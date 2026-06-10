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
		['stat', 'probe', 'delete', 'annotate', 'list', 'crawl'].includes(operation)
	);

	const operationLabels: Record<string, string> = {
		stat: 'Stat',
		probe: 'Probe',
		copy: 'Copy',
		move: 'Move',
		delete: 'Delete',
		list: 'List',
		annotate: 'Annotate',
		crawl: 'Crawl'
	};

	// ---- Crawl sink (docs/32 batch-fold) -----------------------------------
	// `sink: undefined` = stream batches over the `crawl` channel (demo-50
	// shape); `index`/`reconcile` = publish each batch durably to the
	// INVENTORY_FOLD stream, folded set-based into the inventory — the
	// at-scale campaign shape (demo 55). Mirrors CrawlSinkConfig.
	const crawlSink = $derived((config.sink as Record<string, unknown>) ?? null);
	const crawlSinkMode = $derived((crawlSink?.mode as string) ?? '__none__');
	const sinkModeLabels: Record<string, string> = {
		__none__: 'None — stream over the crawl channel',
		index: 'Index — fold into inventory (hashless observe)',
		reconcile: 'Reconcile — classify against the legacy baseline'
	};

	function setSinkMode(v: string) {
		if (v === '__none__') {
			const { sink: _sink, ...rest } = config;
			onchange(rest);
		} else {
			onchange({
				...config,
				sink: { mode: v, file_server_id: (crawlSink?.file_server_id as string) ?? '' }
			});
		}
	}

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
			} else if (v === 'crawl') {
				base.prefix = '';
				base.batch_size = 5000;
				base.stat = true;
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
			<Select.Item value="crawl" label="Crawl" />
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
{:else if operation === 'crawl'}
	<FormField label="Prefix" for="fo-crawl-prefix">
		<Input
			id="fo-crawl-prefix"
			type="text"
			value={(config.prefix as string) ?? ''}
			placeholder="datasets/"
			disabled={readonly}
			oninput={(e) =>
				onchange({ ...config, prefix: (e.currentTarget as HTMLInputElement).value })}
			class="font-mono"
		/>
	</FormField>
	<FormField label="Batch size" for="fo-crawl-batch">
		<Input
			id="fo-crawl-batch"
			type="number"
			min={1}
			value={(config.batch_size as number) ?? 5000}
			placeholder="5000"
			disabled={readonly}
			oninput={(e) => {
				const val = parseInt((e.currentTarget as HTMLInputElement).value);
				onchange({ ...config, batch_size: isNaN(val) ? undefined : val });
			}}
		/>
	</FormField>
	<label class="flex items-center gap-1.5 text-sm text-muted-foreground">
		<Checkbox
			checked={(config.stat as boolean) ?? true}
			disabled={readonly}
			onCheckedChange={(v) => onchange({ ...config, stat: v })}
		/>
		Stat each entry for size + modified time
	</label>
	<FormField label="Max batches (optional — chunk cap for campaigns)" for="fo-crawl-maxb">
		<Input
			id="fo-crawl-maxb"
			type="number"
			min={1}
			value={(config.max_batches as number) ?? ''}
			placeholder="Walk to exhaustion"
			disabled={readonly}
			oninput={(e) => {
				const val = parseInt((e.currentTarget as HTMLInputElement).value);
				onchange({ ...config, max_batches: isNaN(val) ? undefined : val });
			}}
		/>
	</FormField>
	<FormField label="Resume after (optional cursor)" for="fo-crawl-resume">
		<Input
			id="fo-crawl-resume"
			type="text"
			value={(config.resume_from as string) ?? ''}
			placeholder={'{{ campaign.cursor }} or a literal path'}
			disabled={readonly}
			oninput={(e) => {
				const v = (e.currentTarget as HTMLInputElement).value;
				onchange({ ...config, resume_from: v === '' ? undefined : v });
			}}
			class="font-mono"
		/>
		<p class="text-xs text-muted-foreground">
			Accepts a <code>{'{{ slug.field }}'}</code> borrow — e.g. a loop accumulator threading the
			cursor between campaign iterations. Empty on the first iteration means from-the-start.
		</p>
	</FormField>
	<div class="space-y-1.5 rounded-lg border border-border bg-muted/30 p-2">
		<span class="text-sm font-medium text-muted-foreground">Batch sink</span>
		<Select.Root
			type="single"
			value={crawlSinkMode}
			onValueChange={(v) => { if (v) setSinkMode(v); }}
			disabled={readonly}
		>
			<Select.Trigger disabled={readonly} class="h-6 px-1.5 py-0.5 text-sm" data-testid="crawl-sink-mode">
				{sinkModeLabels[crawlSinkMode] ?? crawlSinkMode}
			</Select.Trigger>
			<Select.Content>
				<Select.Item value="__none__" label={sinkModeLabels.__none__} />
				<Select.Item value="index" label={sinkModeLabels.index} />
				<Select.Item value="reconcile" label={sinkModeLabels.reconcile} />
			</Select.Content>
		</Select.Root>
		{#if crawlSinkMode !== '__none__'}
			<Input
				type="text"
				value={(crawlSink?.file_server_id as string) ?? ''}
				placeholder={'campaign-nas or {{ start.file_server }}'}
				disabled={readonly}
				oninput={(e) =>
					onchange({
						...config,
						sink: { mode: crawlSinkMode, file_server_id: (e.currentTarget as HTMLInputElement).value }
					})}
				class="h-6 px-1.5 py-0.5 font-mono text-sm"
				data-testid="crawl-sink-server"
			/>
			<p class="text-xs text-muted-foreground">
				Inventory server key the crawled paths belong to. In sink mode each batch is published
				durably and folded set-based — no per-file tokens through the net, so this is the shape
				for big crawls (see demo 55). No channel/gather wiring needed on this node.
			</p>
		{:else}
			<p class="text-xs text-muted-foreground">
				Batches ride the <code>crawl</code> control channel (wire a consumer with
				<code>join: gather</code>) — fine for small crawls; for big ones use a sink.
			</p>
		{/if}
	</div>
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
