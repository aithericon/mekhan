//! HTTP adapters that send OCR requests to the managed Surya subprocess.
//!
//! Item 1 scaffold declares the `surya` adapter module; Item 2 fills in
//! the per-request HTTP path against the subprocess's `POST /ocr`
//! endpoint.

pub mod surya;
