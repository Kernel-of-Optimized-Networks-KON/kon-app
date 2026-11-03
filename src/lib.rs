//! KON application core. Everything here is platform-independent except `platform`, which isolates OS behavior.
//! The Tauri GUI (`src-tauri/`, excluded from this crate so core CI needs no WebView libs) is a thin shell over `job::JobRunner`; engines stay in their own repositories.
pub mod dng;
pub mod engine;
pub mod ingest;
pub mod job;
pub mod platform;
pub mod viewer;
