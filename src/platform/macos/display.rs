//! macOS display discovery and coordinate clamping utilities.
//!
//! This module provides display topology discovery using Core Graphics APIs.
//! It discovers all active displays, computes virtual desktop bounds, and
//! provides coordinate clamping to ensure injected mouse positions land
//! within valid display regions.
//!
//! # Display Signature
//!
//! The module computes a stable hash signature from display bounds that can
//! be used to detect display topology changes (e.g., monitor reconfiguration).

use anyhow::{Result, anyhow};
use core_graphics::display::CGDisplay;
use core_graphics::event::CGEvent;
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;

use crate::platform::{DisplayBounds, DisplayManager};

/// Bounding box for a single screen in virtual desktop coordinates.
#[derive(Clone, Copy)]
struct ScreenBounds {
    min_x: f64,
    max_x: f64,
    min_y: f64,
    max_y: f64,
}

impl ScreenBounds {
    fn from_display(display: CGDisplay) -> Self {
        let bounds = display.bounds();
        Self::from_raw(
            bounds.origin.x,
            bounds.origin.y,
            bounds.size.width,
            bounds.size.height,
        )
    }

    fn from_raw(origin_x: f64, origin_y: f64, width: f64, height: f64) -> Self {
        let min_x = origin_x;
        let min_y = origin_y;
        let max_x = if width > 0.0 {
            origin_x + width - 1.0
        } else {
            min_x
        };
        let max_y = if height > 0.0 {
            origin_y + height - 1.0
        } else {
            min_y
        };
        Self {
            min_x,
            max_x: max_x.max(min_x),
            min_y,
            max_y: max_y.max(min_y),
        }
    }

    fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.min_x && x <= self.max_x && y >= self.min_y && y <= self.max_y
    }

    fn clamp_f64(&self, x: f64, y: f64) -> CGPoint {
        CGPoint {
            x: x.clamp(self.min_x, self.max_x),
            y: y.clamp(self.min_y, self.max_y),
        }
    }
}

/// Cached display topology with computed virtual bounds and signature.
#[derive(Clone)]
struct DisplayTopology {
    displays: Vec<ScreenBounds>,
    virtual_bounds: ScreenBounds,
    signature: u64,
}

impl DisplayTopology {
    fn discover() -> Self {
        let mut display_ids = match CGDisplay::active_displays() {
            Ok(ids) if !ids.is_empty() => ids,
            _ => vec![CGDisplay::main().id],
        };

        display_ids.sort_unstable();
        display_ids.dedup();

        let displays = display_ids
            .into_iter()
            .map(|id| ScreenBounds::from_display(CGDisplay::new(id)))
            .collect::<Vec<_>>();

        let virtual_bounds = if displays.is_empty() {
            ScreenBounds::from_display(CGDisplay::main())
        } else {
            combined_bounds(&displays)
        };

        let mut signature = 0u64;
        for display in &displays {
            signature = signature.rotate_left(7) ^ display.min_x.to_bits();
            signature = signature.rotate_left(7) ^ display.max_x.to_bits();
            signature = signature.rotate_left(7) ^ display.min_y.to_bits();
            signature = signature.rotate_left(7) ^ display.max_y.to_bits();
        }

        Self {
            displays,
            virtual_bounds,
            signature,
        }
    }

    fn contains(&self, x: f64, y: f64) -> bool {
        self.displays.iter().any(|display| display.contains(x, y))
    }

    fn clamp_point_to_active_display(&self, x: f64, y: f64) -> CGPoint {
        if self.contains(x, y) {
            return CGPoint { x, y };
        }

        let mut best_point = self.virtual_bounds.clamp_f64(x, y);
        let mut best_distance = f64::MAX;
        let mut best_priority = 3;

        for display in &self.displays {
            let candidate = display.clamp_f64(x, y);
            let dx = x - candidate.x;
            let dy = y - candidate.y;
            let distance = dx * dx + dy * dy;

            let contains_x = x >= display.min_x && x <= display.max_x;
            let contains_y = y >= display.min_y && y <= display.max_y;

            let priority = if contains_x {
                1
            } else if contains_y {
                2
            } else {
                3
            };

            if priority < best_priority || (priority == best_priority && distance < best_distance) {
                best_priority = priority;
                best_distance = distance;
                best_point = candidate;
            }
        }

        best_point
    }
}

fn combined_bounds(displays: &[ScreenBounds]) -> ScreenBounds {
    if displays.is_empty() {
        return ScreenBounds::from_display(CGDisplay::main());
    }

    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;

    for display in displays {
        min_x = min_x.min(display.min_x);
        max_x = max_x.max(display.max_x);
        min_y = min_y.min(display.min_y);
        max_y = max_y.max(display.max_y);
    }

    ScreenBounds {
        min_x,
        max_x,
        min_y,
        max_y,
    }
}

/// macOS display manager implementing [`DisplayManager`] using Core Graphics.
///
/// Discovers displays via `CGDisplay::active_displays()` and provides
/// coordinate operations for the platform engine.
pub(crate) struct MacosDisplayManager;

impl MacosDisplayManager {
    pub(crate) fn new() -> Self {
        Self
    }
}

fn to_display_bounds(bounds: ScreenBounds) -> DisplayBounds {
    DisplayBounds {
        x: bounds.min_x.round() as i32,
        y: bounds.min_y.round() as i32,
        width: (bounds.max_x - bounds.min_x + 1.0).max(1.0).round() as u32,
        height: (bounds.max_y - bounds.min_y + 1.0).max(1.0).round() as u32,
    }
}

impl DisplayManager for MacosDisplayManager {
    fn discover_displays(&self) -> Result<Vec<DisplayBounds>> {
        let topology = DisplayTopology::discover();
        Ok(topology
            .displays
            .iter()
            .copied()
            .map(to_display_bounds)
            .collect())
    }

    fn get_virtual_bounds(&self) -> Result<DisplayBounds> {
        let topology = DisplayTopology::discover();
        Ok(to_display_bounds(topology.virtual_bounds))
    }

    fn contains_point(&self, x: i32, y: i32) -> Result<bool> {
        let topology = DisplayTopology::discover();
        Ok(topology.contains(x as f64, y as f64))
    }

    fn clamp_to_active_display(&self, x: i32, y: i32) -> Result<(i32, i32)> {
        let topology = DisplayTopology::discover();
        let point = topology.clamp_point_to_active_display(x as f64, y as f64);
        Ok((point.x.round() as i32, point.y.round() as i32))
    }

    fn get_signature(&self) -> Result<u64> {
        let topology = DisplayTopology::discover();
        Ok(topology.signature)
    }

    fn cursor_position(&self) -> Result<(i32, i32)> {
        let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
            .map_err(|_| anyhow!("Failed to create CGEventSource"))?;
        let event = CGEvent::new(source).map_err(|_| anyhow!("Failed to create CGEvent"))?;
        let loc = event.location();
        Ok((loc.x.round() as i32, loc.y.round() as i32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_virtual_bounds_alignment() {
        let display1 = ScreenBounds::from_raw(0.0, 0.0, 1920.0, 1080.0);
        let display2 = ScreenBounds::from_raw(1920.0, 0.0, 1920.0, 1080.0);
        let virtual_bounds = combined_bounds(&[display1, display2]);

        assert_eq!(virtual_bounds.min_x, 0.0);
        assert_eq!(virtual_bounds.max_x, 3839.0);
        assert_eq!(virtual_bounds.min_y, 0.0);
        assert_eq!(virtual_bounds.max_y, 1079.0);

        let display_bounds = to_display_bounds(virtual_bounds);
        assert_eq!(display_bounds.x, 0);
        assert_eq!(display_bounds.width, 3840);
        assert_eq!(display_bounds.y, 0);
        assert_eq!(display_bounds.height, 1080);
    }

    #[test]
    fn to_display_bounds_preserves_right_and_bottom_edges() {
        let bounds = ScreenBounds::from_raw(0.0, 0.0, 2056.0, 1329.0);
        let display_bounds = to_display_bounds(bounds);

        assert_eq!(display_bounds.width, 2056);
        assert_eq!(display_bounds.height, 1329);
        assert!(bounds.contains(2055.0, 1328.0));
        assert!(!bounds.contains(2056.0, 1329.0));
    }

    #[test]
    fn test_clamp_point_to_active_display_resolution_mismatch() {
        // Left display: 1920x1080
        let left_display = ScreenBounds::from_raw(0.0, 0.0, 1920.0, 1080.0);
        // Right display: 2560x1440
        let right_display = ScreenBounds::from_raw(1920.0, 0.0, 2560.0, 1440.0);

        let virtual_bounds = combined_bounds(&[left_display, right_display]);

        let topology = DisplayTopology {
            displays: vec![left_display, right_display],
            virtual_bounds,
            signature: 0,
        };

        // Inject point from right display moving left: x = 1919 (just crossed boundary), y = 1300
        let clamped = topology.clamp_point_to_active_display(1919.0, 1300.0);

        // Should clamp to the bottom row of the left display, not snap back to the right display.
        assert_eq!(clamped.x, 1919.0);
        assert_eq!(clamped.y, 1079.0);
    }
}
