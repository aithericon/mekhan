/**
 * Client-side resolver: robot model name → loaded URDF text + mesh blobs.
 *
 * The URDF and its meshes live in a `robot_description` asset (docs/20 Asset
 * model). That asset type has (at least) the fields:
 *   - `robot_model` (string)   — the model key callers resolve by (e.g. `xarm6`)
 *   - `urdf`        (File)      — the URDF XML, a `storage_path` value
 *   - `meshes`      (File)      — a zip of the mesh tree, a `storage_path` value
 *
 * A File-field value in a record is a `storage_path` string; the backend serves
 * the bytes at `GET /api/v1/files/{storage_path}` (see
 * `fetchAssetFileText` / `fetchAssetFileBytes` in `$lib/api/assets.ts`). The
 * mesh zip is expanded in-browser with `fflate.unzipSync` into a map keyed by
 * each entry's FULL archive path, e.g.
 *   `xarm_description/meshes/xarm6/visual/link1.stl`
 * so that URDF `package://` / relative mesh refs can be matched against it.
 *
 * Framework-agnostic (no Svelte runtime deps). Results are memoised per
 * `robotModel` at module scope.
 */
import { unzipSync } from 'fflate';
import {
	listAssetTypes,
	listAssets,
	getAsset,
	fetchAssetFileText,
	fetchAssetFileBytes,
	type AssetTypeSummary,
	type AssetSummary,
	type AssetRecord
} from '$lib/api/assets';

/** The asset-type `name` carrying robot URDF + mesh bundles. */
const ROBOT_DESCRIPTION_TYPE_NAME = 'robot_description';
/** Conventional `ref_key` of the seeded robot-descriptions asset. */
const ROBOT_DESCRIPTION_REF_KEY = 'robot_descriptions';

export interface LoadedRobotDescription {
	/** The URDF XML, decoded as text. */
	urdfText: string;
	/** Mesh entry path (full archive path) → raw bytes. */
	meshes: Map<string, Uint8Array>;
}

// Module-level memo, keyed by robotModel. Stores the in-flight promise so
// concurrent callers share one network resolution and never double-fetch.
const cache = new Map<string, Promise<LoadedRobotDescription | null>>();

/**
 * Resolve a robot model name to its loaded URDF text and mesh blobs.
 *
 * @returns the bundle, or `null` if no `robot_description` record matches
 *          `robotModel` (or the asset type / asset is absent).
 */
export function loadRobotDescription(
	robotModel: string
): Promise<LoadedRobotDescription | null> {
	const cached = cache.get(robotModel);
	if (cached) return cached;
	const p = resolve(robotModel).catch((err) => {
		// Don't cache failures — a transient network error shouldn't poison the
		// model name permanently.
		cache.delete(robotModel);
		throw err;
	});
	cache.set(robotModel, p);
	return p;
}

/** Clear the memo (test/teardown helper). */
export function clearRobotDescriptionCache(): void {
	cache.clear();
}

async function resolve(robotModel: string): Promise<LoadedRobotDescription | null> {
	const record = await findRecord(robotModel);
	if (!record) return null;

	const urdfPath = filePath(record, 'urdf');
	const meshesPath = filePath(record, 'meshes');
	if (!urdfPath || !meshesPath) return null;

	const [urdfText, zipBytes] = await Promise.all([
		fetchAssetFileText(urdfPath),
		fetchAssetFileBytes(meshesPath)
	]);

	const meshes = new Map<string, Uint8Array>();
	const unzipped = unzipSync(zipBytes);
	for (const [entryPath, bytes] of Object.entries(unzipped)) {
		// fflate yields directory entries as zero-length values; skip them so the
		// map only holds real files.
		if (entryPath.endsWith('/') && bytes.length === 0) continue;
		meshes.set(entryPath, bytes);
	}

	return { urdfText, meshes };
}

/**
 * Find the asset record whose `robot_model` === `robotModel`, paging through
 * the (single) `robot_description` asset's records.
 *
 * Resolution order:
 *   1. List asset types, pick the one whose `name === 'robot_description'`.
 *   2. List that type's assets; prefer the one with `ref_key ===
 *      'robot_descriptions'`, else take the first.
 *   3. Page its records, returning the first with a matching `robot_model`.
 */
async function findRecord(robotModel: string): Promise<AssetRecord | null> {
	const type = await findRobotDescriptionType();
	if (!type) return null;

	const asset = await findRobotDescriptionAsset(type.id);
	if (!asset) return null;

	const perPage = 200;
	let page = 0;
	for (;;) {
		const detail = await getAsset(asset.id, { page, perPage });
		const records = detail.records as AssetRecord[];
		for (const record of records) {
			if (asString(record['robot_model']) === robotModel) return record;
		}
		const seen = page * perPage + records.length;
		if (records.length === 0 || seen >= detail.record_count) return null;
		page += 1;
	}
}

async function findRobotDescriptionType(): Promise<AssetTypeSummary | null> {
	const perPage = 200;
	let page = 0;
	for (;;) {
		const list = await listAssetTypes({ page, perPage });
		const hit = list.items.find((t) => t.name === ROBOT_DESCRIPTION_TYPE_NAME);
		if (hit) return hit;
		const seen = page * perPage + list.items.length;
		if (list.items.length === 0 || seen >= list.total) return null;
		page += 1;
	}
}

async function findRobotDescriptionAsset(typeId: string): Promise<AssetSummary | null> {
	const perPage = 200;
	let page = 0;
	let first: AssetSummary | null = null;
	for (;;) {
		const list = await listAssets({ type_id: typeId, page, perPage });
		for (const a of list.items) {
			if (a.ref_key === ROBOT_DESCRIPTION_REF_KEY) return a;
			if (!first) first = a;
		}
		const seen = page * perPage + list.items.length;
		if (list.items.length === 0 || seen >= list.total) break;
		page += 1;
	}
	return first;
}

/** Read a File-field value (a `storage_path` string) off a record. */
function filePath(record: AssetRecord, field: string): string | null {
	return asString(record[field]);
}

function asString(v: unknown): string | null {
	return typeof v === 'string' && v.length > 0 ? v : null;
}
