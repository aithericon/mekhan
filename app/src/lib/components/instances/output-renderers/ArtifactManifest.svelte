<script lang="ts">
	import { Badge } from '$lib/components/ui/badge';
	import FileBox from '@lucide/svelte/icons/file-box';
	import ArtifactMediaPreview from '$lib/components/catalogue/ArtifactMediaPreview.svelte';

	// Renders the executor envelope's `detail.artifact_manifest` — the files a
	// step registered via the SDK `log_artifact(...)`. Each entry links to the
	// file catalogue download endpoint and (for image/video/audio) previews
	// inline via `ArtifactMediaPreview`. Mirrors the `Artifact` shape from
	// `executor/crates/executor-domain/src/artifact.rs`.
	type Artifact = {
		id?: string;
		name?: string;
		category?: string;
		filename?: string;
		mime_type?: string | null;
		size_bytes?: number | null;
		storage_path?: string | null;
		metadata?: Record<string, string> | null;
	};
	type Manifest = {
		execution_id?: string;
		artifacts?: Artifact[];
	};

	let { manifest }: { manifest: unknown } = $props();

	const artifacts = $derived<Artifact[]>(
		manifest && typeof manifest === 'object' && Array.isArray((manifest as Manifest).artifacts)
			? ((manifest as Manifest).artifacts as Artifact[])
			: []
	);
</script>

{#if artifacts.length > 0}
	<div>
		<div class="mb-1.5 flex items-center gap-1.5 text-sm font-semibold text-foreground">
			<FileBox class="size-3.5 text-muted-foreground" />
			Artifacts
			<Badge variant="secondary" class="font-mono text-sm">{artifacts.length}</Badge>
		</div>
		<div class="space-y-2">
			{#each artifacts as a, i (a.id ?? a.storage_path ?? i)}
				<div class="rounded-md border border-border bg-muted/20 p-3">
					<div class="mb-1.5 flex flex-wrap items-center gap-2 text-sm">
						{#if a.name}
							<span class="truncate font-medium text-foreground">{a.name}</span>
						{/if}
						{#if a.category}
							<Badge variant="outline" class="font-mono text-sm">{a.category}</Badge>
						{/if}
					</div>
					<ArtifactMediaPreview
						storagePath={a.storage_path ?? null}
						mimeType={a.mime_type ?? null}
						filename={a.filename}
						name={a.name}
						sizeBytes={a.size_bytes}
					/>
				</div>
			{/each}
		</div>
	</div>
{/if}
