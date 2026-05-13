# executor-kreuzberg

Document text extraction backend for the Aithericon Executor, powered by [kreuzberg](https://github.com/nicholasgasior/kreuzberg) — a Rust library that extracts text, metadata, and tables from 75+ file formats.

## Supported Formats

PDF, Word (.docx), Excel (.xlsx), PowerPoint (.pptx), images (with OCR via Tesseract or PaddleOCR), email (.eml, .msg), HTML, XML, archives (.zip, .tar.gz), and plain text — among others. Format support depends on which feature flags are enabled.

## Feature Flags

| Feature | Description |
|---|---|
| `pdf` | PDF extraction (requires system poppler) |
| `ocr` | OCR for images and scanned documents |
| `excel` | Excel spreadsheet extraction |
| `office` | Word/PowerPoint extraction |
| `email` | Email (.eml, .msg) extraction |
| `html` | HTML content extraction |
| `xml` | XML content extraction |
| `archives` | Archive (.zip, .tar.gz) extraction |
| `language-detection` | Auto-detect document language |
| `chunking` | Text chunking for downstream processing |
| `quality` | Content quality scoring |
| `full` | All features enabled |

Default: no features (text-only extraction). Enable features as needed:

```bash
cargo build -p aithericon-executor-kreuzberg --features pdf,ocr
```

To enable in the executor service:

```bash
cargo build -p aithericon-executor-service --features kreuzberg
```

## Configuration

The `spec.config` object accepts:

| Field | Type | Default | Description |
|---|---|---|---|
| `mode` | `string` | `"single"` | `"single"` or `"batch"`. |
| `file` | `string` | `null` | Input name for single mode. Auto-resolved if omitted (sole input or `"file"`). |
| `files` | `[string]` | `[]` | Input names for batch mode. Empty = all staged inputs. |
| `mime_type` | `string` | `null` | MIME type override. Auto-detected from file extension if absent. |
| `force_ocr` | `bool` | `false` | Force OCR even on text-based PDFs. |
| `ocr` | `OcrSettings` | `null` | OCR configuration (see below). |
| `pdf` | `PdfSettings` | `null` | PDF-specific settings (see below). |

### OcrSettings

| Field | Type | Default | Description |
|---|---|---|---|
| `backend` | `string` | `"tesseract"` | OCR backend: `"tesseract"` or `"paddle-ocr"`. |
| `language` | `string` | `"eng"` | Language code (ISO 639-3). |
| `enable_table_detection` | `bool` | `false` | Enable table detection during OCR. |

### PdfSettings

| Field | Type | Default | Description |
|---|---|---|---|
| `passwords` | `[string]` | `null` | Passwords for encrypted PDFs (tried in order). |

## Outputs

### Single Mode

| Key | Type | Description |
|---|---|---|
| `content` | `string` | Extracted text content. |
| `mime_type` | `string` | Detected MIME type of the source file. |
| `tables` | `[object]` | Extracted tables with `markdown`, `page_number`, and `rows` fields. |
| `table_count` | `number` | Number of tables found. |
| `word_count` | `number` | Word count of extracted content. |
| `char_count` | `number` | Character count of extracted content. |
| `detected_languages` | `[string]` | Detected languages (if language-detection feature enabled). |
| `metadata` | `object` | Kreuzberg's native extraction metadata. |

Undeclared spec outputs are mapped to `content` (like the LLM backend maps to `response`).

### Batch Mode

| Key | Type | Description |
|---|---|---|
| `results` | `[object]` | Array of per-file results, each with `file`, `content`, `tables`, `word_count`, etc. |
| `total_files` | `number` | Total files processed. |
| `successful` | `number` | Files successfully extracted. |
| `failed` | `number` | Files that failed extraction. |
| `errors` | `[object]` | Array of `{file, error}` for failed files. |

Batch mode reports per-file progress via the status callback. Partial failures (some files succeed, some fail) are `Success`; total failure (all files fail) is `BackendError`.

## Metrics

### Single Mode

- `kreuzberg/extraction_time_ms`
- `kreuzberg/content_length`
- `kreuzberg/word_count`
- `kreuzberg/table_count`

### Batch Mode

- `kreuzberg/total_extraction_time_ms`
- `kreuzberg/total_files`
- `kreuzberg/successful_files`
- `kreuzberg/failed_files`
- `kreuzberg/total_content_length`
- `kreuzberg/total_table_count`

## Example Jobs

### Minimal single-file extraction

```json
{
  "execution_id": "extract-001",
  "spec": {
    "type": "kreuzberg",
    "inputs": [
      { "name": "file", "source": { "type": "storage_path", "path": "uploads/contract.pdf" } }
    ],
    "outputs": [
      { "name": "content", "required": true }
    ],
    "config": {}
  }
}
```

### OCR with password-protected PDF

```json
{
  "execution_id": "extract-002",
  "spec": {
    "type": "kreuzberg",
    "inputs": [
      { "name": "document", "source": { "type": "storage_path", "path": "scanned/invoice.pdf" } }
    ],
    "config": {
      "file": "document",
      "force_ocr": true,
      "ocr": { "backend": "tesseract", "language": "eng", "enable_table_detection": true },
      "pdf": { "passwords": ["secret123"] }
    }
  }
}
```

### Batch extraction

```json
{
  "execution_id": "extract-003",
  "spec": {
    "type": "kreuzberg",
    "inputs": [
      { "name": "report_q1", "source": { "type": "storage_path", "path": "reports/q1.pdf" } },
      { "name": "report_q2", "source": { "type": "storage_path", "path": "reports/q2.pdf" } },
      { "name": "spreadsheet", "source": { "type": "storage_path", "path": "data/financials.xlsx" } }
    ],
    "outputs": [
      { "name": "results", "required": true }
    ],
    "config": {
      "mode": "batch"
    }
  }
}
```

## Testing

```bash
# Unit + integration tests (text extraction only, no features needed)
cargo test -p aithericon-executor-kreuzberg

# With PDF support
cargo test -p aithericon-executor-kreuzberg --features pdf
```
