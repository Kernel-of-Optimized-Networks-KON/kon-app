//! KON application core. Everything here is platform-independent except `platform`, which isolates OS behavior.
//! The GUI (not yet chosen/added) is a thin shell over `job::JobRunner`; engines stay in their own repositories.
pub mod engine;
pub mod job;
pub mod platform;
