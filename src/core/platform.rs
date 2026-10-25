//! Platform abstraction layer for input capture, injection, and display management.
//!
//! This module provides the core traits and types that allow the mouse-sharing
//! engine to run on multiple operating systems. It decouples the high-level
//! logic (networking, transitions, CRDT) from the low-level OS APIs.
//!
//! # Key Components
//!
//! - [`traits`]: Interface definitions for OS subsystems.
//! - [`types`]: Platform-agnostic coordinates and bounds.
//! - [`engine`]: The high-level interface for controlling the mouse-sharing lifecycle.

pub mod engine;
pub mod traits;
pub mod types;

#[cfg(test)]
mod tests;

pub use engine::{
    EdgeAssignments, PlatformEngine, PlatformEngineTestExt, RuntimeEngine, SessionEngine,
    TestPlatformEngine,
};
pub use traits::{
    DisplayManager, EventCapture, EventInjection, PermissionChecker, ThroughputMetrics,
};
pub use types::{
    CapturePolicy, CapturedInputEvent, DisplayBounds, LocalCoords, ModifierMapping, ModifierTarget,
    PeerEdge, PlatformFamily, Point, TransferMode, VirtualCoords, WINDOWS_LINES_PER_NOTCH,
    scale_wheel_for_target, translate_hid_for_target_platform,
};
