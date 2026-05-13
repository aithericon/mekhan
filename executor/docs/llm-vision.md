# LLM Vision & OCR Guide

The LLM backend supports sending images alongside text prompts for vision and OCR tasks. This guide covers setup, configuration, supported models, and provider-specific details.

## Overview

Vision support works by:

1. **Staging** image files as job inputs (via `InputDeclaration`).
2. **Referencing** them in the `images` config field using `{{input:NAME}}` templates.
3. The backend **reads** each file, **base64-encodes** it, and includes it in the provider-specific API request alongside the text prompt.

Images are always attached to the final user message in the conversation.

## Quick Start

### Minimal OCR Job (Ollama + GLM-OCR)

```json
{
  "execution_id": "ocr-invoice-001",
  "spec": {
    "type": "llm",
    "inputs": [
      {
        "name": "invoice.png",
        "source": { "type": "storage_path", "path": "uploads/invoice-2024-001.png" }
      }
    ],
    "outputs": [],
    "config": {
      "provider": "ollama",
      "model": "glm-ocr:q8_0",
      "prompt": "Extract all text from this document image.",
      "images": [
        { "path": "{{input:invoice.png}}" }
      ]
    }
  }
}
```

### Structured OCR with JSON Schema

```json
{
  "execution_id": "ocr-invoice-structured",
  "spec": {
    "type": "llm",
    "inputs": [
      {
        "name": "invoice.png",
        "source": { "type": "storage_path", "path": "uploads/invoice-2024-001.png" }
      }
    ],
    "outputs": [],
    "config": {
      "provider": "ollama",
      "model": "glm-ocr:q8_0",
      "prompt": "Extract the invoice number, date, line items, and total amount.",
      "images": [
        { "path": "{{input:invoice.png}}" }
      ],
      "response_format": {
        "type": "json_schema",
        "schema": {
          "type": "object",
          "properties": {
            "invoice_number": { "type": "string" },
            "date": { "type": "string" },
            "line_items": {
              "type": "array",
              "items": {
                "type": "object",
                "properties": {
                  "description": { "type": "string" },
                  "quantity": { "type": "number" },
                  "unit_price": { "type": "number" }
                }
              }
            },
            "total": { "type": "number" }
          },
          "required": ["invoice_number", "total"]
        }
      }
    }
  }
}
```

### Cloud Vision (OpenAI GPT-4o)

```json
{
  "execution_id": "describe-photo",
  "spec": {
    "type": "llm",
    "inputs": [
      {
        "name": "photo.jpg",
        "source": { "type": "storage_path", "path": "uploads/beach-photo.jpg" }
      }
    ],
    "outputs": [],
    "config": {
      "provider": "open_ai",
      "model": "gpt-4o",
      "prompt": "Describe this photograph in detail. What objects, people, and scenery are visible?",
      "images": [
        { "path": "{{input:photo.jpg}}" }
      ],
      "max_tokens": 1024
    }
  }
}
```

### Multi-Image Analysis (Anthropic Claude)

```json
{
  "execution_id": "compare-designs",
  "spec": {
    "type": "llm",
    "inputs": [
      {
        "name": "design_v1.png",
        "source": { "type": "storage_path", "path": "designs/v1.png" }
      },
      {
        "name": "design_v2.png",
        "source": { "type": "storage_path", "path": "designs/v2.png" }
      }
    ],
    "outputs": [],
    "config": {
      "provider": "anthropic",
      "model": "claude-sonnet-4-20250514",
      "prompt": "Compare these two UI designs. What changed between v1 and v2?",
      "images": [
        { "path": "{{input:design_v1.png}}" },
        { "path": "{{input:design_v2.png}}" }
      ],
      "max_tokens": 2048
    }
  }
}
```

## Configuration Reference

### ImageInput

| Field | Type | Default | Description |
|---|---|---|---|
| `path` | `string` | required | Path to the image file. Use `{{input:NAME}}` to reference a staged input. |
| `media_type` | `string` | auto-detected | MIME type. If absent, guessed from file extension. |

### Supported Image Formats

| Extension | MIME Type |
|---|---|
| `.png` | `image/png` |
| `.jpg`, `.jpeg` | `image/jpeg` |
| `.gif` | `image/gif` |
| `.webp` | `image/webp` |
| `.bmp` | `image/bmp` |
| `.tiff`, `.tif` | `image/tiff` |

Files with unrecognized extensions are sent as `application/octet-stream`. For best compatibility, use PNG or JPEG.

### Image Sources

Images must be staged via `InputDeclaration`. Supported input sources:

| Source | Example | Use Case |
|---|---|---|
| `storage_path` | `{ "type": "storage_path", "path": "artifacts/scan.png" }` | Images stored in the artifact store (S3, GCS, local). |
| `inline` | `{ "type": "inline", "value": "<base64>" }` | Small images embedded directly (uncommon). |
| `raw` | `{ "type": "raw", "content": "<binary>" }` | Raw binary content (uncommon). |

The most common pattern is `storage_path` — the executor downloads the file from the artifact store, stages it into the run directory's `inputs/` folder, and the LLM backend reads it from there.

## Supported Models

### Local (Ollama)

| Model | Size (q8_0) | Best For | Ollama Tag |
|---|---|---|---|
| GLM-OCR | 1.6 GB | Document OCR, text extraction | `glm-ocr:q8_0` |
| LLaVA 1.6 | 5.0 GB | General vision, image understanding | `llava:13b` |
| LLaVA-Phi3 | 2.9 GB | Lightweight vision | `llava-phi3` |
| Moondream | 1.7 GB | Lightweight image captioning | `moondream` |
| BakLLaVA | 4.7 GB | General vision | `bakllava` |

GLM-OCR is recommended for document OCR — it is small (0.9B params), fast, and specifically trained for text extraction from images.

Pull models ahead of time:
```bash
ollama pull glm-ocr:q8_0
```

### Cloud Providers

| Provider | Models | Notes |
|---|---|---|
| **OpenAI** | `gpt-4o`, `gpt-4o-mini`, `gpt-4-turbo` | Images sent as `data:` URIs in multi-content messages. |
| **Anthropic** | `claude-sonnet-4-20250514`, `claude-haiku-4-5-20251001` | Images sent as base64 `source` blocks. Anthropic places images before text in the content array. |

Cloud providers require API keys — set via `api_key` in config or provider-specific env vars (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`).

## Provider-Specific Details

### Ollama

Ollama uses a flat `images` array on each message — raw base64 strings with no data URI wrapper:

```json
{
  "model": "glm-ocr:q8_0",
  "messages": [{
    "role": "user",
    "content": "Extract text from this image.",
    "images": ["iVBORw0KGgo..."]
  }]
}
```

Ollama defaults to `http://localhost:11434`. Override with `base_url`:

```json
{
  "provider": "ollama",
  "model": "glm-ocr:q8_0",
  "base_url": "http://ollama-server:11434",
  "prompt": "Extract all text.",
  "images": [{ "path": "{{input:doc.png}}" }]
}
```

### OpenAI

OpenAI uses multi-content messages with `image_url` parts containing `data:` URIs:

```json
{
  "model": "gpt-4o",
  "messages": [{
    "role": "user",
    "content": [
      { "type": "text", "text": "Extract text from this image." },
      { "type": "image_url", "image_url": { "url": "data:image/png;base64,iVBORw0KGgo..." } }
    ]
  }]
}
```

Text-only messages are sent as plain strings for backward compatibility. Messages with images automatically switch to the multi-content array format.

### Anthropic

Anthropic uses multi-content messages with `image` source blocks. Images are placed before the text content (Anthropic convention):

```json
{
  "model": "claude-sonnet-4-20250514",
  "messages": [{
    "role": "user",
    "content": [
      { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "iVBORw0KGgo..." } },
      { "type": "text", "text": "Extract text from this image." }
    ]
  }]
}
```

## Pipeline Patterns

### OCR → Structured Extraction

Use `response_format` with `json_schema` to get structured OCR output:

```json
{
  "spec": {
    "type": "llm",
    "inputs": [
      { "name": "receipt.jpg", "source": { "type": "storage_path", "path": "scans/receipt-042.jpg" } }
    ],
    "outputs": [
      { "name": "extracted.json" }
    ],
    "config": {
      "provider": "ollama",
      "model": "glm-ocr:q8_0",
      "prompt": "Extract the merchant name, date, items purchased, and total from this receipt.",
      "images": [{ "path": "{{input:receipt.jpg}}" }],
      "response_format": {
        "type": "json_schema",
        "schema": {
          "type": "object",
          "properties": {
            "merchant": { "type": "string" },
            "date": { "type": "string" },
            "items": { "type": "array", "items": { "type": "string" } },
            "total": { "type": "string" }
          },
          "required": ["merchant", "total"]
        }
      }
    }
  }
}
```

The structured response is available in `outputs["response"]` as a parsed JSON object and written to any declared output files.

### Batch Document Processing

Process multiple documents by submitting separate jobs per document (one image per job for OCR models), or multiple images per job for comparison tasks:

```json
{
  "spec": {
    "type": "llm",
    "inputs": [
      { "name": "page1.png", "source": { "type": "storage_path", "path": "docs/contract-p1.png" } },
      { "name": "page2.png", "source": { "type": "storage_path", "path": "docs/contract-p2.png" } },
      { "name": "page3.png", "source": { "type": "storage_path", "path": "docs/contract-p3.png" } }
    ],
    "config": {
      "provider": "open_ai",
      "model": "gpt-4o",
      "prompt": "These are consecutive pages of a contract. Extract the key terms, parties involved, and any monetary amounts.",
      "images": [
        { "path": "{{input:page1.png}}" },
        { "path": "{{input:page2.png}}" },
        { "path": "{{input:page3.png}}" }
      ],
      "max_tokens": 4096
    }
  }
}
```

### Inline Text + Image

Combine staged text inputs with images using `{{input:NAME}}` in the prompt:

```json
{
  "spec": {
    "type": "llm",
    "inputs": [
      { "name": "context.txt", "source": { "type": "raw", "content": "This invoice is from Acme Corp, customer ID #4521." } },
      { "name": "invoice.png", "source": { "type": "storage_path", "path": "uploads/invoice.png" } }
    ],
    "config": {
      "provider": "ollama",
      "model": "glm-ocr:q8_0",
      "prompt": "Context: {{input:context.txt}}\n\nExtract the line items and totals from this invoice image.",
      "images": [{ "path": "{{input:invoice.png}}" }]
    }
  }
}
```

## Outputs

Vision jobs produce the same outputs as text-only LLM jobs:

| Key | Type | Description |
|---|---|---|
| `response` | `json` | LLM response: text string (text mode) or parsed JSON object (json_schema mode). |
| `usage` | `object` | Token usage: `input_tokens`, `output_tokens`, `total_tokens`. Image tokens are included in `input_tokens`. |
| `finish_reason` | `string` | `stop`, `length`, `content_filter`, or other. |
| `model` | `string` | Model identifier returned by the provider. |

## Error Handling

| Error | Cause | Resolution |
|---|---|---|
| `failed to read image 'path'` | Image file not found or unreadable. | Ensure the input is declared in `inputs` and the `{{input:NAME}}` reference matches. |
| Provider 4xx/5xx | Model doesn't support images, file too large, or invalid format. | Check model capabilities and image size limits. |
| `invalid llm backend config` | Malformed `images` array in config JSON. | Each entry needs at least a `path` field. |

## Tips

- **File size**: Keep images reasonable. Most providers have limits (e.g., OpenAI: 20MB per image, Anthropic: 5MB). For OCR, 300 DPI scans typically work well.
- **MIME type**: Usually auto-detected from file extension. Override with `media_type` if the extension is missing or incorrect.
- **Timeouts**: OCR and vision models can be slower than text-only models. Set an appropriate `timeout` on the job (e.g., `"5m"` for large images or multiple pages).
- **Local vs. cloud**: For high-volume OCR, local Ollama with GLM-OCR avoids per-token API costs. For complex visual reasoning, cloud models (GPT-4o, Claude) are more capable.
- **Structured output**: Combining `images` with `response_format: json_schema` is the most reliable way to extract structured data from documents.
