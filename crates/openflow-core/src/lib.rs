//! Everything OpenFlow does that does not draw a window.
//!
//! The Tauri shell and the AppKit binary are both thin hosts over this crate:
//! they own windows, trays and menus, and delegate capture, transcription,
//! insertion, storage and secrets to the modules below.

pub mod audio;
pub mod db;
pub mod plugins;
pub mod secrets;
pub mod transcribe;
