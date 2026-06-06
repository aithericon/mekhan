//! Container `.sif` reference derivation — byte-exact parity with the engine.
//!
//! The mekhan compiler embeds a by-ref `.sif` path into each containerized
//! scheduled step's [`CompilerContainerSpec`](super::CompilerContainerSpec).
//! The engine's Slurm allocator resolves the *same* path from the image ref it
//! receives, so the sanitization here MUST stay byte-for-byte identical to the
//! engine fn (`engine/core-engine/crates/slurm/src/alloc.rs::sanitize_image_ref`)
//! and the const layout (`SHARED_SIF_ROOT = "/shared/sif"`,
//! `engine/core-engine/crates/api/src/slurm_allocator.rs`). A drift here means
//! mekhan stages/expects a path the engine never produces (or vice versa), so
//! the unit tests below pin the engine's published test vectors.

/// Engine shared-SIF root. Mirrors `SHARED_SIF_ROOT` in
/// `engine/core-engine/crates/api/src/slurm_allocator.rs`.
pub const SHARED_SIF_ROOT: &str = "/shared/sif";

/// Sanitize an image ref into a filesystem-safe stem. BYTE-EXACT copy of the
/// engine fn (`engine/core-engine/crates/slurm/src/alloc.rs`): collapse every
/// run of non-`[A-Za-z0-9]` characters to a single `_`, then trim leading and
/// trailing `_`.
pub fn sanitize_image_ref(image_ref: &str) -> String {
    let mut out = String::with_capacity(image_ref.len());
    let mut prev_us = false;
    for ch in image_ref.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            prev_us = false;
        } else if !prev_us {
            out.push('_');
            prev_us = true;
        }
    }
    out.trim_matches('_').to_string()
}

/// The deterministic by-ref `.sif` path the compiler embeds and the engine
/// resolves: `<SHARED_SIF_ROOT>/by-ref/<sanitized>.sif`.
pub fn by_ref_sif_path(image_ref: &str) -> String {
    format!(
        "{SHARED_SIF_ROOT}/by-ref/{}.sif",
        sanitize_image_ref(image_ref)
    )
}

/// The per-image venv-cache bind path: `/shared/venv-cache/<sanitized>`. Listed
/// last in the container `binds` the compiler emits (the engine augments
/// nothing — it binds exactly the list it receives).
pub fn venv_cache_bind(image_ref: &str) -> String {
    format!("/shared/venv-cache/{}", sanitize_image_ref(image_ref))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_matches_engine_vectors() {
        assert_eq!(
            sanitize_image_ref("ghcr.io/org/img:tag"),
            "ghcr_io_org_img_tag"
        );
        assert_eq!(sanitize_image_ref("python:3.12-slim"), "python_3_12_slim");
        assert_eq!(sanitize_image_ref("a@@b"), "a_b");
    }

    #[test]
    fn by_ref_path_for_python_slim() {
        assert_eq!(
            by_ref_sif_path("python:3.12-slim"),
            "/shared/sif/by-ref/python_3_12_slim.sif"
        );
    }

    #[test]
    fn venv_cache_bind_path() {
        assert_eq!(
            venv_cache_bind("python:3.12-slim"),
            "/shared/venv-cache/python_3_12_slim"
        );
    }
}
