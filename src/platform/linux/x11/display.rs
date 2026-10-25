//! X11 display topology discovery and coordinate helpers.

use anyhow::{Result, anyhow};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::platform::{DisplayBounds, DisplayManager};
use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as RandrConnectionExt;
use x11rb::protocol::xproto::ConnectionExt as XprotoConnectionExt;

#[derive(Clone, Copy)]
struct MonitorRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl MonitorRect {
    fn from_xywh(x: i32, y: i32, width: u32, height: u32) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        Self {
            left: x,
            top: y,
            right: x.saturating_add(width as i32),
            bottom: y.saturating_add(height as i32),
        }
    }

    fn width(self) -> u32 {
        (self.right - self.left).max(1) as u32
    }

    fn height(self) -> u32 {
        (self.bottom - self.top).max(1) as u32
    }

    fn as_bounds(self) -> DisplayBounds {
        DisplayBounds {
            x: self.left,
            y: self.top,
            width: self.width(),
            height: self.height(),
        }
    }

    fn contains(self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }

    fn clamp(self, x: i32, y: i32) -> (i32, i32) {
        let max_x = self.right.saturating_sub(1).max(self.left);
        let max_y = self.bottom.saturating_sub(1).max(self.top);
        (x.clamp(self.left, max_x), y.clamp(self.top, max_y))
    }
}

#[derive(Clone)]
struct DisplayTopology {
    monitors: Vec<MonitorRect>,
    virtual_bounds: MonitorRect,
}

impl DisplayTopology {
    fn discover() -> Result<Self> {
        let (conn, screen_num) = x11rb::connect(None)
            .map_err(|err| anyhow!("failed to connect to X11 display server: {err}"))?;
        let setup = conn.setup();
        let root = setup
            .roots
            .get(screen_num)
            .ok_or_else(|| anyhow!("X11 screen index out of bounds"))?
            .root;

        let mut monitors = discover_randr_monitors(&conn, root)?;
        if monitors.is_empty() {
            let fallback = fallback_root_rect(&conn, screen_num, root)?;
            monitors.push(fallback);
        }

        let virtual_bounds = combine_rects(&monitors);
        Ok(Self {
            monitors,
            virtual_bounds,
        })
    }

    fn signature(&self) -> u64 {
        let mut ordered = self.monitors.clone();
        ordered.sort_unstable_by_key(|r| (r.left, r.top, r.right, r.bottom));

        let mut hasher = DefaultHasher::new();
        for rect in ordered {
            rect.left.hash(&mut hasher);
            rect.top.hash(&mut hasher);
            rect.right.hash(&mut hasher);
            rect.bottom.hash(&mut hasher);
        }
        hasher.finish()
    }

    fn contains(&self, x: i32, y: i32) -> bool {
        self.monitors.iter().any(|rect| rect.contains(x, y))
    }

    fn clamp_to_active(&self, x: i32, y: i32) -> (i32, i32) {
        if self.contains(x, y) {
            return (x, y);
        }

        let mut best = self.virtual_bounds.clamp(x, y);
        let mut best_dist = i64::MAX;
        for rect in &self.monitors {
            let candidate = rect.clamp(x, y);
            let dx = i64::from(x) - i64::from(candidate.0);
            let dy = i64::from(y) - i64::from(candidate.1);
            let dist = dx * dx + dy * dy;
            if dist < best_dist {
                best_dist = dist;
                best = candidate;
            }
        }

        best
    }
}

pub(crate) struct X11DisplayManager;

impl X11DisplayManager {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl DisplayManager for X11DisplayManager {
    fn discover_displays(&self) -> Result<Vec<DisplayBounds>> {
        let topology = DisplayTopology::discover()?;
        Ok(topology
            .monitors
            .iter()
            .copied()
            .map(MonitorRect::as_bounds)
            .collect())
    }

    fn get_virtual_bounds(&self) -> Result<DisplayBounds> {
        let topology = DisplayTopology::discover()?;
        Ok(topology.virtual_bounds.as_bounds())
    }

    fn contains_point(&self, x: i32, y: i32) -> Result<bool> {
        let topology = DisplayTopology::discover()?;
        Ok(topology.contains(x, y))
    }

    fn clamp_to_active_display(&self, x: i32, y: i32) -> Result<(i32, i32)> {
        let topology = DisplayTopology::discover()?;
        Ok(topology.clamp_to_active(x, y))
    }

    fn get_signature(&self) -> Result<u64> {
        let topology = DisplayTopology::discover()?;
        Ok(topology.signature())
    }

    fn cursor_position(&self) -> Result<(i32, i32)> {
        let (conn, screen_num) = x11rb::connect(None)
            .map_err(|err| anyhow!("failed to connect to X11 display server: {err}"))?;
        let root = conn
            .setup()
            .roots
            .get(screen_num)
            .ok_or_else(|| anyhow!("X11 screen index out of bounds"))?
            .root;
        let cookie = conn.query_pointer(root)?;
        let reply = cookie.reply()?;
        Ok((i32::from(reply.root_x), i32::from(reply.root_y)))
    }

}

#[allow(dead_code)]
pub(crate) fn screen_resolution_string() -> Result<String> {
    let manager = X11DisplayManager::new();
    let bounds = manager.get_virtual_bounds()?;
    Ok(format!("{}x{}", bounds.width as i32, bounds.height as i32))
}

fn discover_randr_monitors<C: Connection>(conn: &C, root: u32) -> Result<Vec<MonitorRect>> {
    let randr_present = conn
        .query_extension(b"RANDR")?
        .reply()
        .map(|reply| reply.present)
        .unwrap_or(false);
    if !randr_present {
        return Ok(Vec::new());
    }

    let reply = match conn.randr_get_monitors(root, true)?.reply() {
        Ok(reply) => reply,
        Err(_) => return Ok(Vec::new()),
    };

    let mut monitors = Vec::with_capacity(reply.monitors.len());
    for monitor in reply.monitors {
        monitors.push(MonitorRect::from_xywh(
            monitor.x as i32,
            monitor.y as i32,
            monitor.width as u32,
            monitor.height as u32,
        ));
    }
    Ok(monitors)
}

fn fallback_root_rect<C: Connection>(
    conn: &C,
    screen_num: usize,
    root: u32,
) -> Result<MonitorRect> {
    if let Some(screen) = conn.setup().roots.get(screen_num) {
        return Ok(MonitorRect::from_xywh(
            0,
            0,
            screen.width_in_pixels.into(),
            screen.height_in_pixels.into(),
        ));
    }

    let geometry = conn.get_geometry(root)?.reply()?;
    Ok(MonitorRect::from_xywh(
        geometry.x as i32,
        geometry.y as i32,
        geometry.width as u32,
        geometry.height as u32,
    ))
}

fn combine_rects(monitors: &[MonitorRect]) -> MonitorRect {
    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;

    for rect in monitors {
        min_x = min_x.min(rect.left);
        min_y = min_y.min(rect.top);
        max_x = max_x.max(rect.right);
        max_y = max_y.max(rect.bottom);
    }

    if min_x == i32::MAX {
        return MonitorRect::from_xywh(0, 0, 1, 1);
    }

    MonitorRect {
        left: min_x,
        top: min_y,
        right: max_x,
        bottom: max_y,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combine_rects_handles_negative_origins() {
        let monitors = vec![
            MonitorRect::from_xywh(-1920, 0, 1920, 1080),
            MonitorRect::from_xywh(0, 0, 2560, 1440),
            MonitorRect::from_xywh(2560, -200, 1200, 1200),
        ];

        let combined = combine_rects(&monitors).as_bounds();
        assert_eq!(combined.x, -1920);
        assert_eq!(combined.y, -200);
        assert_eq!(combined.width, 5680);
        assert_eq!(combined.height, 1640);
    }

    #[test]
    fn clamp_to_active_chooses_nearest_monitor() {
        let left = MonitorRect::from_xywh(-1920, 0, 1920, 1080);
        let right = MonitorRect::from_xywh(0, 0, 1920, 1080);
        let topology = DisplayTopology {
            monitors: vec![left, right],
            virtual_bounds: combine_rects(&[left, right]),
        };

        assert_eq!(topology.clamp_to_active(-2500, 40), (-1920, 40));
        assert_eq!(topology.clamp_to_active(2300, 90), (1919, 90));
        assert_eq!(topology.clamp_to_active(-50, 2000), (-50, 1079));
    }

    #[test]
    #[serial_test::serial]
    fn live_topology_is_self_consistent() {
        let has_display = std::env::var("DISPLAY")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .is_some();
        if !has_display {
            return;
        }
        let started = std::time::Instant::now();
        while x11rb::connect(None).is_err() {
            if started.elapsed() >= std::time::Duration::from_secs(20) {
                eprintln!("SKIP live_topology_is_self_consistent: no live X display");
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let manager = X11DisplayManager::new();
        let displays = manager
            .discover_displays()
            .expect("live topology discovery should succeed");
        assert!(!displays.is_empty());
        let virtual_bounds = manager
            .get_virtual_bounds()
            .expect("virtual bounds should succeed");
        for display in &displays {
            assert!(display.width > 0);
            assert!(display.height > 0);
            assert!(display.x >= virtual_bounds.x);
            assert!(display.y >= virtual_bounds.y);
        }
        let first = &displays[0];
        let center = (
            first.x + first.width as i32 / 2,
            first.y + first.height as i32 / 2,
        );
        assert!(manager
            .contains_point(center.0, center.1)
            .expect("contains check should succeed"));
        let clamped = manager
            .clamp_to_active_display(1_000_000, 1_000_000)
            .expect("clamp should succeed");
        assert!(manager
            .contains_point(clamped.0, clamped.1)
            .expect("clamped point check should succeed"));
        let (cursor_x, cursor_y) = manager
            .cursor_position()
            .expect("cursor position should succeed");
        assert!(manager
            .contains_point(cursor_x, cursor_y)
            .expect("cursor containment check should succeed"));
        let first_signature = manager.get_signature().expect("signature should succeed");
        assert_eq!(first_signature, manager.get_signature().unwrap());
        assert!(!screen_resolution_string()
            .expect("resolution string should succeed")
            .is_empty());
    }

    #[test]
    fn signature_is_stable_regardless_of_monitor_order() {
        let a = MonitorRect::from_xywh(0, 0, 1920, 1080);
        let b = MonitorRect::from_xywh(1920, 0, 1920, 1080);

        let topology_a = DisplayTopology {
            monitors: vec![a, b],
            virtual_bounds: combine_rects(&[a, b]),
        };
        let topology_b = DisplayTopology {
            monitors: vec![b, a],
            virtual_bounds: combine_rects(&[a, b]),
        };

        assert_eq!(topology_a.signature(), topology_b.signature());
    }
}
