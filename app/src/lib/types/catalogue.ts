export interface CatalogueEntry {
	id: string;
	execution_id: string;
	job_id: string | null;
	name: string;
	category: string;
	filename: string;
	mime_type: string | null;
	size_bytes: number | null;
	storage_path: string | null;
	source_net: string | null;
	source_place: string | null;
	correlation_id: string | null;
	process_id: string | null;
	process_step: string | null;
	file_metadata: Record<string, unknown>;
	user_metadata: Record<string, unknown>;
	created_at: string;
	catalogued_at: string;
}

export interface CatalogueListResponse {
	entries: CatalogueEntry[];
	total: number;
	limit: number;
	offset: number;
}

export interface CatalogueStats {
	total_entries: number;
	total_size_bytes: number;
	by_category: { category: string; count: number; total_bytes: number }[];
	latest_at: string | null;
}

export interface CatalogueNetStats {
	source_net: string | null;
	total_artifacts: number;
	total_bytes: number;
	first_at: string | null;
	latest_at: string | null;
}
