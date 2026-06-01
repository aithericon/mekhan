"""Surya OCR pool-server — FastAPI wrapper for managed-subprocess invocation.

Bundled with `aithericon-executor-surya`. Spawned by Rust at
`SuryaSubprocess::start()` via `python -m surya_pool_server --port <N>`.

## License

This wrapper script is Apache-2.0 (our code). The bundled `surya-ocr`
dep is GPL-3.0 + modified OpenRAIL-M weights; subprocess
process-isolation contains GPL-3.0 — the wrapper invokes Surya in-process
within the venv, never in the Rust binary's address space. See
`../src/surya_subprocess.rs` § "License isolation" for the architectural
rationale.

## Wire contract

- ``GET /health`` → ``{status, models_loaded, device}`` — used by
  ``SuryaSubprocess::wait_for_ready`` to poll readiness and by
  ``SuryaSubprocess::health_check`` for runtime liveness.
- ``POST /ocr`` body ``{file_base64, mime_type}`` → ``{full_text, pages,
  engine, device, processing_time_ms}`` — used by
  ``SuryaAdapter::ocr``. Field names mirror the legacy
  ``online-clinic/ocr/src/models.py`` envelope (``file_base64`` +
  ``mime_type`` request; ``full_text`` + ``pages`` response) so the two
  wrappers can swap with minimal coordination during the legacy-sidecar
  transition.

## Device selection

The wrapper honours ``SURYA_DEVICE`` env (``cpu`` / ``cuda`` / ``mps`` /
``auto``) forwarded from Rust's ``SuryaSubprocessConfig::device``. When
unset / ``auto``, PyTorch auto-detection runs (``cuda`` → ``mps`` →
``cpu`` priority).
"""

from __future__ import annotations

import argparse
import base64
import io
import logging
import os
import sys
import time
from contextlib import asynccontextmanager
from typing import Any, Optional

import uvicorn
from fastapi import FastAPI, HTTPException
from pydantic import BaseModel, Field

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# Engine lifecycle
# ---------------------------------------------------------------------------

_engine: Optional["SuryaEngine"] = None


def _resolve_device(device_hint: Optional[str]) -> str:
    """Resolve the PyTorch device string. Honours ``SURYA_DEVICE`` env or
    the per-call hint; falls back to natural auto-detection."""
    hint = device_hint or os.environ.get("SURYA_DEVICE") or "auto"
    if hint != "auto":
        return hint

    try:
        import torch  # type: ignore[import-not-found]
    except Exception:  # pragma: no cover — torch absent only on broken venv
        return "cpu"

    if torch.cuda.is_available():
        return "cuda"
    if (
        hasattr(torch.backends, "mps")
        and torch.backends.mps.is_available()
    ):
        return "mps"
    return "cpu"


class SuryaEngine:
    """Holds the Surya predictors. Single-instance per subprocess (default
    uvicorn single-worker mode keeps PID-scope clean for the Rust
    ``SuryaSubprocess::stop()`` SIGKILL path)."""

    def __init__(self, device: str) -> None:
        self.device = device
        self.models_loaded = False
        logger.info("Initializing Surya OCR on device: %s", self.device)

        # Imports are deferred to instantiation time so import errors
        # surface during readiness (where they become health-probe
        # failures) rather than at module-import time (where they'd
        # crash before uvicorn even binds the port).
        from surya.detection import DetectionPredictor  # type: ignore[import-not-found]
        from surya.foundation import FoundationPredictor  # type: ignore[import-not-found]
        from surya.layout import LayoutPredictor  # type: ignore[import-not-found]
        from surya.recognition import RecognitionPredictor  # type: ignore[import-not-found]

        self._det = DetectionPredictor(device=self.device)
        foundation = FoundationPredictor(device=self.device)
        self._rec = RecognitionPredictor(foundation)
        self._layout = LayoutPredictor(foundation)
        self.models_loaded = True
        logger.info("Surya models loaded successfully")

    def process(self, images: list) -> dict[str, Any]:
        """Run the full OCR pipeline: recognition (with built-in detection)
        returns per-page word/line/bbox data; this wrapper flattens to the
        legacy ``{pages, full_text, processing_time_ms, device, model}``
        envelope."""
        start = time.monotonic()
        rec_results = self._rec(
            images,
            det_predictor=self._det,
            return_words=True,
        )

        pages: list[dict[str, Any]] = []
        full_text_parts: list[str] = []
        global_word_index = 0

        for page_idx, (image, rec_result) in enumerate(zip(images, rec_results)):
            width_px, height_px = image.size
            words: list[dict[str, Any]] = []
            lines: list[dict[str, Any]] = []
            page_text_parts: list[str] = []

            for line in rec_result.text_lines:
                line_text = line.text
                line_bbox = _bbox_to_pct(line.bbox, width_px, height_px)
                line_word_indices: list[int] = []

                surya_words = getattr(line, "words", None)
                if surya_words:
                    for word in surya_words:
                        word_bbox = _bbox_to_pct(word.bbox, width_px, height_px)
                        words.append(
                            {
                                "text": word.text,
                                "bbox": word_bbox,
                                "confidence": word.confidence or 0.9,
                                "word_index": global_word_index,
                            }
                        )
                        line_word_indices.append(global_word_index)
                        global_word_index += 1

                lines.append(
                    {
                        "text": line_text,
                        "bbox": line_bbox,
                        "word_indices": line_word_indices,
                    }
                )
                page_text_parts.append(line_text)

            full_text_parts.append("\n".join(page_text_parts))
            pages.append(
                {
                    "page_number": page_idx + 1,
                    "width_px": width_px,
                    "height_px": height_px,
                    "words": words,
                    "lines": lines,
                }
            )

        elapsed_ms = int((time.monotonic() - start) * 1000)
        return {
            "pages": pages,
            "full_text": "\n\n".join(full_text_parts),
            "processing_time_ms": elapsed_ms,
            "device": self.device,
            "model": "surya",
        }


def _bbox_to_pct(
    bbox: list[float], width_px: float, height_px: float
) -> dict[str, float]:
    x1, y1, x2, y2 = bbox
    return {
        "x": x1 / width_px,
        "y": y1 / height_px,
        "w": (x2 - x1) / width_px,
        "h": (y2 - y1) / height_px,
    }


# ---------------------------------------------------------------------------
# File → images
# ---------------------------------------------------------------------------


def _file_to_images(file_bytes: bytes, mime_type: str) -> list:
    """Decode bytes to a list of PIL images. PDFs use pdf2image (which
    requires the system ``poppler`` binary, same as the legacy ocr/
    sidecar)."""
    from PIL import Image  # type: ignore[import-not-found]

    if mime_type == "application/pdf":
        from pdf2image import convert_from_bytes  # type: ignore[import-not-found]

        return convert_from_bytes(file_bytes, dpi=300)
    if mime_type in ("image/jpeg", "image/jpg", "image/png", "image/tiff", "image/webp"):
        return [Image.open(io.BytesIO(file_bytes)).convert("RGB")]
    raise ValueError(f"Unsupported mime type: {mime_type}")


# ---------------------------------------------------------------------------
# FastAPI app
# ---------------------------------------------------------------------------


@asynccontextmanager
async def lifespan(app: FastAPI):
    global _engine
    device = _resolve_device(None)
    logger.info("Starting Surya pool-server on device: %s", device)
    try:
        _engine = SuryaEngine(device=device)
    except Exception as exc:  # pragma: no cover — surface for operators
        logger.error("Failed to initialize Surya engine: %s", exc, exc_info=True)
        _engine = None
    yield
    logger.info("Shutting down Surya pool-server")


app = FastAPI(title="Surya OCR pool-server", lifespan=lifespan)


class HealthResponse(BaseModel):
    status: str
    models_loaded: bool
    device: str
    message: Optional[str] = None


class OcrRequest(BaseModel):
    file_base64: str = Field(..., description="Base64-encoded input bytes")
    mime_type: str = Field(..., description="Input MIME type")


@app.get("/health", response_model=HealthResponse)
async def health() -> HealthResponse:
    if _engine is None or not _engine.models_loaded:
        return HealthResponse(
            status="error",
            models_loaded=False,
            device="unknown",
            message="Models not loaded",
        )
    return HealthResponse(
        status="ok",
        models_loaded=True,
        device=_engine.device,
    )


@app.post("/ocr")
async def ocr(request: OcrRequest) -> dict[str, Any]:
    if _engine is None:
        raise HTTPException(status_code=503, detail="OCR engine not initialized")
    try:
        file_bytes = base64.b64decode(request.file_base64)
    except Exception as exc:
        raise HTTPException(status_code=400, detail=f"base64 decode: {exc}") from exc
    if not file_bytes:
        raise HTTPException(status_code=400, detail="empty input after base64 decode")

    try:
        images = _file_to_images(file_bytes, request.mime_type)
    except ValueError as exc:
        raise HTTPException(status_code=422, detail=str(exc)) from exc
    if not images:
        raise HTTPException(
            status_code=422, detail="no images could be extracted from file"
        )

    try:
        return _engine.process(images)
    except Exception as exc:
        logger.error("OCR processing failed: %s", exc, exc_info=True)
        raise HTTPException(
            status_code=500, detail=f"OCR processing failed: {exc}"
        ) from exc


# ---------------------------------------------------------------------------
# Entry point — invoked by Rust via `python -m surya_pool_server --port N`
# ---------------------------------------------------------------------------


def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser(prog="surya_pool_server")
    parser.add_argument("--port", type=int, default=7160)
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument(
        "--log-level",
        default=os.environ.get("SURYA_LOG_LEVEL", "info"),
        choices=["critical", "error", "warning", "info", "debug", "trace"],
    )
    args = parser.parse_args(argv)

    logging.basicConfig(
        level=getattr(logging, args.log_level.upper(), logging.INFO),
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )

    # Single-worker mode preserves PID-scope for the Rust
    # `SuryaSubprocess::stop()` SIGKILL path; multi-worker would fork
    # children that survive the parent's SIGKILL until kernel reaper
    # cleans them up.
    uvicorn.run(
        "surya_pool_server:app",
        host=args.host,
        port=args.port,
        workers=1,
        log_level=args.log_level,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
