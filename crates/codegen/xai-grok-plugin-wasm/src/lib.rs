//! WASM plugin runtime (scaffold — de-risking offline dependency resolution).
#![forbid(unsafe_code)]

/// Placeholder to confirm the crate and its `wasmi` dependency resolve offline.
pub fn runtime_name() -> &'static str {
    "xai-grok-plugin-wasm"
}
