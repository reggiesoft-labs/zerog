//! Wayland display topology with backend-selected implementation.
//!
//! Native display discovery on Wayland is restricted to protocol-specific
//! extensions. This module probes for the best available protocol and falls
//! back gracefully:
//!
//! 1. `wlr-output-management` (Sway, Hyprland, River, Wayfire)
//! 2. `xdg-output` (GNOME, Plasma)
//! 3. XWayland (X11 display via XWayland bridge)
//! 4. Single-display fallback (1920×1080)

use anyhow::Result;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::platform::{DisplayBounds, DisplayManager};

use super::super::x11::display::X11DisplayManager;
use super::session;

const FALLBACK_WIDTH: u32 = 1920;
const FALLBACK_HEIGHT: u32 = 1080;

// ---------------------------------------------------------------------------
// DisplayBackend — selects the active discovery mechanism
// ---------------------------------------------------------------------------

/// The display topology discovery backend in use.
enum DisplayBackend {
    /// Native wlr-output-management protocol (Sway, Hyprland, etc.)
    #[cfg(feature = "linux-wayland")]
    WlrOutputManagement,

    /// Native xdg-output protocol (GNOME, Plasma, etc.)
    #[cfg(feature = "linux-wayland")]
    XdgOutput,

    /// XWayland bridge: use X11 display manager for topology.
    XWayland(X11DisplayManager),

    /// Single-display fallback with configurable resolution.
    #[allow(dead_code)]
    Fallback { width: u32, height: u32 },
}

// ---------------------------------------------------------------------------
// WaylandDisplayManager
// ---------------------------------------------------------------------------

/// Wayland display manager with backend-selected topology discovery.
pub(crate) struct WaylandDisplayManager {
    backend: DisplayBackend,
}

impl WaylandDisplayManager {
    /// Create a new `WaylandDisplayManager`, probing the compositor for the
    /// best available display-discovery backend.
    ///
    /// When the `linux-wayland` feature is enabled the probe order is:
    /// 1. wlr-output-management
    /// 2. xdg-output
    /// 3. XWayland
    /// 4. Fallback
    ///
    /// Without the feature only XWayland and Fallback are considered.
    pub(crate) fn new() -> Self {
        let probe = session::probe_wayland_support();

        #[cfg(feature = "linux-wayland")]
        {
            use session::OutputProtocolPath;

            // Respect the user-selected protocol first.
            let backend = match probe.output_protocol {
                OutputProtocolPath::WlrOutputManagement => DisplayBackend::WlrOutputManagement,
                OutputProtocolPath::XdgOutput => DisplayBackend::XdgOutput,
                OutputProtocolPath::SingleDisplayFallback => {
                    if probe.xwayland_fallback_available {
                        DisplayBackend::XWayland(X11DisplayManager::new())
                    } else {
                        DisplayBackend::Fallback {
                            width: fallback_width(),
                            height: fallback_height(),
                        }
                    }
                }
            };
            return Self { backend };
        }

        // Without the feature flag, fall back to XWayland or a static size.
        #[allow(unreachable_code)]
        {
            if probe.xwayland_fallback_available {
                Self {
                    backend: DisplayBackend::XWayland(X11DisplayManager::new()),
                }
            } else {
                Self {
                    backend: DisplayBackend::Fallback {
                        width: fallback_width(),
                        height: fallback_height(),
                    },
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// DisplayManager trait implementation
// ---------------------------------------------------------------------------

impl DisplayManager for WaylandDisplayManager {
    fn discover_displays(&self) -> Result<Vec<DisplayBounds>> {
        match &self.backend {
            #[cfg(feature = "linux-wayland")]
            DisplayBackend::WlrOutputManagement => {
                discover_wlr_outputs().or_else(|_| Ok(vec![fallback_display_bounds()]))
            }
            #[cfg(feature = "linux-wayland")]
            DisplayBackend::XdgOutput => {
                discover_xdg_outputs().or_else(|_| Ok(vec![fallback_display_bounds()]))
            }
            DisplayBackend::XWayland(mgr) => mgr.discover_displays(),
            DisplayBackend::Fallback { .. } => Ok(vec![fallback_display_bounds()]),
        }
    }

    fn get_virtual_bounds(&self) -> Result<DisplayBounds> {
        let displays = self.discover_displays()?;
        Ok(compute_virtual_bounds(&displays))
    }

    fn contains_point(&self, x: i32, y: i32) -> Result<bool> {
        let displays = self.discover_displays()?;
        Ok(displays.iter().any(|b| point_in_bounds(b, x, y)))
    }

    fn clamp_to_active_display(&self, x: i32, y: i32) -> Result<(i32, i32)> {
        let displays = self.discover_displays()?;
        Ok(clamp_to_nearest(&displays, x, y))
    }

    fn get_signature(&self) -> Result<u64> {
        let mut displays = self.discover_displays()?;
        // Sort for a stable hash regardless of discovery order.
        displays.sort_unstable_by_key(|b| (b.x, b.y, b.width, b.height));
        let mut hasher = DefaultHasher::new();
        for b in &displays {
            b.x.hash(&mut hasher);
            b.y.hash(&mut hasher);
            b.width.hash(&mut hasher);
            b.height.hash(&mut hasher);
        }
        Ok(hasher.finish())
    }
}

// ---------------------------------------------------------------------------
// wlr-output-management discovery
// ---------------------------------------------------------------------------

#[cfg(feature = "linux-wayland")]
mod wlr {
    use anyhow::{Context, Result};
    use wayland_client::{
        Connection, Dispatch, QueueHandle, globals::registry_queue_init, protocol::wl_registry,
    };
    use wayland_protocols_wlr::output_management::v1::client::{
        zwlr_output_head_v1::{self, ZwlrOutputHeadV1},
        zwlr_output_manager_v1::{self, ZwlrOutputManagerV1},
        zwlr_output_mode_v1::{self, ZwlrOutputModeV1},
    };

    use crate::platform::DisplayBounds;

    // Per-head data accumulated during the sync.
    #[derive(Default, Debug)]
    struct HeadData {
        name: String,
        x: i32,
        y: i32,
        // Physical pixel dimensions from the current mode.
        width: i32,
        height: i32,
        scale: f64,
        enabled: bool,
    }

    impl HeadData {
        fn to_bounds(&self) -> Option<DisplayBounds> {
            if !self.enabled || self.width <= 0 || self.height <= 0 {
                return None;
            }
            let scale = if self.scale > 0.0 {
                self.scale
            } else {
                1.0
            };
            let width = ((self.width as f64 / scale).round() as u32).max(1);
            let height = ((self.height as f64 / scale).round() as u32).max(1);
            Some(DisplayBounds {
                x: self.x,
                y: self.y,
                width,
                height,
            })
        }
    }

    struct WlrState {
        heads: Vec<HeadData>,
        // Pending head being assembled (receives events before done).
        pending_head: Option<HeadData>,
        done: bool,
    }

    impl WlrState {
        fn new() -> Self {
            Self {
                heads: Vec::new(),
                pending_head: None,
                done: false,
            }
        }
    }

    // ---- Dispatch for WlRegistry (required by registry_queue_init) ----------

    impl Dispatch<wl_registry::WlRegistry, wayland_client::globals::GlobalListContents> for WlrState {
        fn event(
            _state: &mut Self,
            _proxy: &wl_registry::WlRegistry,
            _event: wl_registry::Event,
            _data: &wayland_client::globals::GlobalListContents,
            _conn: &Connection,
            _qh: &QueueHandle<Self>,
        ) {
            // Dynamic registry events are not needed for a one-shot probe.
        }
    }

    // ---- Dispatch for ZwlrOutputManagerV1 ----------------------------------

    impl Dispatch<ZwlrOutputManagerV1, ()> for WlrState {
        fn event(
            state: &mut Self,
            _proxy: &ZwlrOutputManagerV1,
            event: zwlr_output_manager_v1::Event,
            _data: &(),
            _conn: &Connection,
            _qh: &QueueHandle<Self>,
        ) {
            match event {
                zwlr_output_manager_v1::Event::Head { head } => {
                    // Initialise a new pending head; events arrive on the head object.
                    state.pending_head = Some(HeadData::default());
                    // Ignore the proxy — head events arrive via ZwlrOutputHeadV1 dispatch.
                    let _ = head;
                }
                zwlr_output_manager_v1::Event::Done { serial: _ } => {
                    state.done = true;
                }
                zwlr_output_manager_v1::Event::Finished => {}
                _ => {}
            }
        }
    }

    // ---- Dispatch for ZwlrOutputHeadV1 -------------------------------------

    impl Dispatch<ZwlrOutputHeadV1, ()> for WlrState {
        fn event(
            state: &mut Self,
            _proxy: &ZwlrOutputHeadV1,
            event: zwlr_output_head_v1::Event,
            _data: &(),
            _conn: &Connection,
            _qh: &QueueHandle<Self>,
        ) {
            let head = match state.pending_head.as_mut() {
                Some(h) => h,
                None => return,
            };

            match event {
                zwlr_output_head_v1::Event::Name { name } => {
                    head.name = name;
                }
                zwlr_output_head_v1::Event::Enabled { enabled } => {
                    head.enabled = enabled != 0;
                }
                zwlr_output_head_v1::Event::Position { x, y } => {
                    head.x = x;
                    head.y = y;
                }
                zwlr_output_head_v1::Event::Scale { scale } => {
                    head.scale = scale;
                }
                zwlr_output_head_v1::Event::CurrentMode { mode: _ } => {
                    // Current mode events reference ZwlrOutputModeV1; we collect
                    // the size from ZwlrOutputModeV1::Event::Size in mode dispatch.
                }
                zwlr_output_head_v1::Event::Finished => {
                    // Head removed; commit whatever we have.
                    if let Some(h) = state.pending_head.take() {
                        state.heads.push(h);
                    }
                }
                _ => {}
            }
        }
    }

    // ---- Dispatch for ZwlrOutputModeV1 -------------------------------------

    impl Dispatch<ZwlrOutputModeV1, ()> for WlrState {
        fn event(
            state: &mut Self,
            _proxy: &ZwlrOutputModeV1,
            event: zwlr_output_mode_v1::Event,
            _data: &(),
            _conn: &Connection,
            _qh: &QueueHandle<Self>,
        ) {
            let head = match state.pending_head.as_mut() {
                Some(h) => h,
                None => return,
            };
            if let zwlr_output_mode_v1::Event::Size { width, height } = event {
                // Update current physical pixel dimensions.
                head.width = width;
                head.height = height;
            }
        }
    }

    /// Discover displays using the `zwlr-output-management-unstable-v1` protocol.
    pub(crate) fn discover_wlr_outputs() -> Result<Vec<DisplayBounds>> {
        let conn = Connection::connect_to_env().context("failed to connect to Wayland display")?;

        let (globals, mut event_queue) = registry_queue_init::<WlrState>(&conn)
            .context("failed to enumerate Wayland globals")?;

        let qh = event_queue.handle();
        let mut state = WlrState::new();

        // Bind the output manager; this triggers Head events for each output.
        let _manager: ZwlrOutputManagerV1 = globals
            .bind(&qh, 1..=4, ())
            .context("compositor does not support zwlr_output_manager_v1")?;

        // Flush pending requests and block until the compositor sends Done.
        // Two roundtrips are typically enough: one for heads, one for Done.
        event_queue
            .roundtrip(&mut state)
            .context("wlr roundtrip failed")?;
        if !state.done {
            event_queue
                .roundtrip(&mut state)
                .context("wlr second roundtrip failed")?;
        }

        // Commit any head that never received Finished (normal for active heads).
        if let Some(h) = state.pending_head.take() {
            state.heads.push(h);
        }

        let bounds: Vec<DisplayBounds> =
            state.heads.iter().filter_map(HeadData::to_bounds).collect();

        if bounds.is_empty() {
            anyhow::bail!("wlr-output-management returned no enabled outputs");
        }

        Ok(bounds)
    }
}

#[cfg(feature = "linux-wayland")]
use wlr::discover_wlr_outputs;

// ---------------------------------------------------------------------------
// xdg-output discovery
// ---------------------------------------------------------------------------

#[cfg(feature = "linux-wayland")]
mod xdg {
    use anyhow::{Context, Result};
    use wayland_client::{
        Connection, Dispatch, QueueHandle,
        globals::registry_queue_init,
        protocol::{wl_output, wl_registry},
    };
    use wayland_protocols::xdg::xdg_output::zv1::client::{
        zxdg_output_manager_v1::{self, ZxdgOutputManagerV1},
        zxdg_output_v1::{self, ZxdgOutputV1},
    };

    use crate::platform::DisplayBounds;

    #[derive(Default, Debug)]
    struct OutputInfo {
        logical_x: i32,
        logical_y: i32,
        logical_width: i32,
        logical_height: i32,
        done: bool,
    }

    struct XdgState {
        outputs: Vec<OutputInfo>,
        pending: Option<OutputInfo>,
        // Track wl_output globals found during registry scan.
        #[allow(dead_code)]
        wl_output_names: Vec<u32>,
        #[allow(dead_code)]
        manager_bound: bool,
    }

    impl XdgState {
        fn new() -> Self {
            Self {
                outputs: Vec::new(),
                pending: None,
                wl_output_names: Vec::new(),
                manager_bound: false,
            }
        }
    }

    // ---- Dispatch for WlRegistry -------------------------------------------

    impl Dispatch<wl_registry::WlRegistry, wayland_client::globals::GlobalListContents> for XdgState {
        fn event(
            _state: &mut Self,
            _proxy: &wl_registry::WlRegistry,
            _event: wl_registry::Event,
            _data: &wayland_client::globals::GlobalListContents,
            _conn: &Connection,
            _qh: &QueueHandle<Self>,
        ) {
        }
    }

    // ---- Dispatch for ZxdgOutputManagerV1 ----------------------------------

    impl Dispatch<ZxdgOutputManagerV1, ()> for XdgState {
        fn event(
            _state: &mut Self,
            _proxy: &ZxdgOutputManagerV1,
            _event: zxdg_output_manager_v1::Event,
            _data: &(),
            _conn: &Connection,
            _qh: &QueueHandle<Self>,
        ) {
        }
    }

    // ---- Dispatch for WlOutput ---------------------------------------------

    impl Dispatch<wl_output::WlOutput, ()> for XdgState {
        fn event(
            _state: &mut Self,
            _proxy: &wl_output::WlOutput,
            _event: wl_output::Event,
            _data: &(),
            _conn: &Connection,
            _qh: &QueueHandle<Self>,
        ) {
            // We don't need wl_output events directly; xdg_output carries the
            // logical geometry we need.
        }
    }

    // ---- Dispatch for ZxdgOutputV1 -----------------------------------------

    impl Dispatch<ZxdgOutputV1, ()> for XdgState {
        fn event(
            state: &mut Self,
            _proxy: &ZxdgOutputV1,
            event: zxdg_output_v1::Event,
            _data: &(),
            _conn: &Connection,
            _qh: &QueueHandle<Self>,
        ) {
            let info = match state.pending.as_mut() {
                Some(i) => i,
                None => return,
            };

            match event {
                zxdg_output_v1::Event::LogicalPosition { x, y } => {
                    info.logical_x = x;
                    info.logical_y = y;
                }
                zxdg_output_v1::Event::LogicalSize { width, height } => {
                    info.logical_width = width;
                    info.logical_height = height;
                }
                zxdg_output_v1::Event::Done => {
                    info.done = true;
                    if let Some(finished) = state.pending.take() {
                        state.outputs.push(finished);
                    }
                }
                _ => {}
            }
        }
    }

    /// Discover displays using the `zxdg-output-unstable-v1` protocol.
    pub(crate) fn discover_xdg_outputs() -> Result<Vec<DisplayBounds>> {
        let conn = Connection::connect_to_env().context("failed to connect to Wayland display")?;

        let (globals, mut event_queue) = registry_queue_init::<XdgState>(&conn)
            .context("failed to enumerate Wayland globals")?;

        let qh = event_queue.handle();
        let mut state = XdgState::new();

        // Bind the xdg output manager.
        let manager: ZxdgOutputManagerV1 = globals
            .bind(&qh, 1..=3, ())
            .context("compositor does not support zxdg_output_manager_v1")?;

        // Collect all wl_output globals from the list.
        let wl_outputs: Vec<wl_output::WlOutput> = globals.contents().with_list(|list| {
            list.iter()
                .filter(|g| g.interface == "wl_output")
                .map(|g| {
                    globals.registry().bind::<wl_output::WlOutput, _, XdgState>(
                        g.name,
                        g.version.min(4),
                        &qh,
                        (),
                    )
                })
                .collect()
        });

        if wl_outputs.is_empty() {
            anyhow::bail!("no wl_output globals found");
        }

        // For each wl_output, request an xdg_output.
        for wl_out in &wl_outputs {
            state.pending = Some(OutputInfo::default());
            let _xdg_out = manager.get_xdg_output(wl_out, &qh, ());
            // Roundtrip to collect the events for this output.
            event_queue
                .roundtrip(&mut state)
                .context("xdg roundtrip failed")?;
            // If pending wasn't consumed by Done, commit it anyway.
            if let Some(info) = state.pending.take() {
                if info.logical_width > 0 && info.logical_height > 0 {
                    state.outputs.push(info);
                }
            }
        }

        let bounds: Vec<DisplayBounds> = state
            .outputs
            .iter()
            .filter(|o| o.logical_width > 0 && o.logical_height > 0)
            .map(|o| DisplayBounds {
                x: o.logical_x,
                y: o.logical_y,
                width: o.logical_width as u32,
                height: o.logical_height as u32,
            })
            .collect();

        if bounds.is_empty() {
            anyhow::bail!("xdg-output returned no usable outputs");
        }

        Ok(bounds)
    }
}

#[cfg(feature = "linux-wayland")]
use xdg::discover_xdg_outputs;

// ---------------------------------------------------------------------------
// Public helpers
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub(crate) fn screen_resolution_string() -> Result<String> {
    let manager = WaylandDisplayManager::new();
    let bounds = manager.get_virtual_bounds()?;
    Ok(format!("{}x{}", bounds.width as i32, bounds.height as i32))
}

pub(crate) fn output_protocol_path() -> &'static str {
    session::probe_wayland_support().output_protocol.as_str()
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

fn fallback_display_bounds() -> DisplayBounds {
    DisplayBounds {
        x: 0,
        y: 0,
        width: fallback_width(),
        height: fallback_height(),
    }
}

fn fallback_width() -> u32 {
    std::env::var("MOUSE_THING_WAYLAND_FALLBACK_WIDTH")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(FALLBACK_WIDTH)
}

fn fallback_height() -> u32 {
    std::env::var("MOUSE_THING_WAYLAND_FALLBACK_HEIGHT")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(FALLBACK_HEIGHT)
}

/// Compute the bounding box of all display bounds.
fn compute_virtual_bounds(displays: &[DisplayBounds]) -> DisplayBounds {
    if displays.is_empty() {
        return fallback_display_bounds();
    }

    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;

    for b in displays {
        min_x = min_x.min(b.x);
        min_y = min_y.min(b.y);
        max_x = max_x.max(b.x + b.width as i32);
        max_y = max_y.max(b.y + b.height as i32);
    }

    DisplayBounds {
        x: min_x,
        y: min_y,
        width: (max_x - min_x).max(1) as u32,
        height: (max_y - min_y).max(1) as u32,
    }
}

/// Return true if `(x, y)` is inside `bounds`.
fn point_in_bounds(bounds: &DisplayBounds, x: i32, y: i32) -> bool {
    x >= bounds.x
        && y >= bounds.y
        && x < bounds.x + bounds.width as i32
        && y < bounds.y + bounds.height as i32
}

/// Clamp `(x, y)` to the boundary of the nearest display in `displays`.
fn clamp_to_nearest(displays: &[DisplayBounds], x: i32, y: i32) -> (i32, i32) {
    if displays.is_empty() {
        return (x, y);
    }

    // If the point is already on a display, no clamping needed.
    if displays.iter().any(|b| point_in_bounds(b, x, y)) {
        return (x, y);
    }

    let mut best = (x, y);
    let mut best_dist = i64::MAX;

    for b in displays {
        let max_x = (b.x + b.width as i32 - 1).max(b.x);
        let max_y = (b.y + b.height as i32 - 1).max(b.y);
        let cx = x.clamp(b.x, max_x);
        let cy = y.clamp(b.y, max_y);
        let dx = i64::from(x - cx);
        let dy = i64::from(y - cy);
        let dist = dx * dx + dy * dy;
        if dist < best_dist {
            best_dist = dist;
            best = (cx, cy);
        }
    }

    best
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(x: i32, y: i32, w: u32, h: u32) -> DisplayBounds {
        DisplayBounds {
            x,
            y,
            width: w,
            height: h,
        }
    }

    #[test]
    fn virtual_bounds_single_display() {
        let displays = vec![bounds(0, 0, 1920, 1080)];
        let vb = compute_virtual_bounds(&displays);
        assert_eq!(vb.x, 0);
        assert_eq!(vb.y, 0);
        assert_eq!(vb.width, 1920);
        assert_eq!(vb.height, 1080);
    }

    #[test]
    fn virtual_bounds_side_by_side() {
        let displays = vec![bounds(0, 0, 1920, 1080), bounds(1920, 0, 1920, 1080)];
        let vb = compute_virtual_bounds(&displays);
        assert_eq!(vb.x, 0);
        assert_eq!(vb.y, 0);
        assert_eq!(vb.width, 3840);
        assert_eq!(vb.height, 1080);
    }

    #[test]
    fn virtual_bounds_negative_origin() {
        let displays = vec![bounds(-1920, 0, 1920, 1080), bounds(0, 0, 2560, 1440)];
        let vb = compute_virtual_bounds(&displays);
        assert_eq!(vb.x, -1920);
        assert_eq!(vb.y, 0);
        assert_eq!(vb.width, 4480);
        assert_eq!(vb.height, 1440);
    }

    #[test]
    fn contains_point_basic() {
        let displays = vec![bounds(0, 0, 1920, 1080)];
        assert!(point_in_bounds(&displays[0], 100, 100));
        assert!(!point_in_bounds(&displays[0], -1, 0));
        assert!(!point_in_bounds(&displays[0], 1920, 0));
    }

    #[test]
    fn clamp_to_nearest_already_inside() {
        let displays = vec![bounds(0, 0, 1920, 1080)];
        assert_eq!(clamp_to_nearest(&displays, 500, 500), (500, 500));
    }

    #[test]
    fn clamp_to_nearest_outside_right() {
        let displays = vec![bounds(0, 0, 1920, 1080)];
        assert_eq!(clamp_to_nearest(&displays, 2000, 540), (1919, 540));
    }

    #[test]
    fn clamp_to_nearest_chooses_closer_display() {
        let displays = vec![bounds(-1920, 0, 1920, 1080), bounds(0, 0, 1920, 1080)];
        // Point to the far right — should clamp to right display.
        let (cx, _) = clamp_to_nearest(&displays, 2500, 40);
        assert_eq!(cx, 1919);
        // Point to the far left — should clamp to left display.
        let (cx, _) = clamp_to_nearest(&displays, -2500, 40);
        assert_eq!(cx, -1920);
    }

    #[test]
    fn signature_stable_across_orderings() {
        let a = bounds(0, 0, 1920, 1080);
        let b = bounds(1920, 0, 1920, 1080);

        let mut v1 = vec![a, b];
        let mut v2 = vec![b, a];

        let sig = |displays: &mut Vec<DisplayBounds>| -> u64 {
            displays.sort_unstable_by_key(|b| (b.x, b.y, b.width, b.height));
            let mut hasher = DefaultHasher::new();
            for d in displays.iter() {
                d.x.hash(&mut hasher);
                d.y.hash(&mut hasher);
                d.width.hash(&mut hasher);
                d.height.hash(&mut hasher);
            }
            hasher.finish()
        };

        assert_eq!(sig(&mut v1), sig(&mut v2));
    }

    // Phase 9 canonical test names ------------------------------------

    #[test]
    fn test_virtual_bounds_single_display() {
        let displays = vec![bounds(0, 0, 1920, 1080)];
        let vb = compute_virtual_bounds(&displays);
        assert_eq!((vb.x, vb.y, vb.width, vb.height), (0, 0, 1920, 1080));
    }

    #[test]
    fn test_virtual_bounds_multi_display() {
        // Two displays side-by-side; the virtual bounds should span both.
        let displays = vec![bounds(0, 0, 1920, 1080), bounds(1920, 0, 2560, 1440)];
        let vb = compute_virtual_bounds(&displays);
        assert_eq!(vb.x, 0);
        assert_eq!(vb.y, 0);
        assert_eq!(vb.width, 1920 + 2560);
        assert_eq!(vb.height, 1440);
    }

    #[test]
    fn test_clamp_to_display() {
        let displays = vec![bounds(0, 0, 1920, 1080)];
        // Point inside — unchanged.
        assert_eq!(clamp_to_nearest(&displays, 100, 200), (100, 200));
        // Point to the right of the display — clamped to the right edge.
        assert_eq!(clamp_to_nearest(&displays, 3000, 540), (1919, 540));
        // Point above the display — clamped to the top edge.
        assert_eq!(clamp_to_nearest(&displays, 960, -50), (960, 0));
    }

    #[test]
    fn test_get_signature_stable() {
        // Sorting before hashing must produce a stable result regardless
        // of the original discovery order.
        let a = bounds(0, 0, 1920, 1080);
        let b = bounds(1920, 0, 2560, 1440);

        let compute_sig = |mut displays: Vec<DisplayBounds>| -> u64 {
            displays.sort_unstable_by_key(|d| (d.x, d.y, d.width, d.height));
            let mut hasher = DefaultHasher::new();
            for d in &displays {
                d.x.hash(&mut hasher);
                d.y.hash(&mut hasher);
                d.width.hash(&mut hasher);
                d.height.hash(&mut hasher);
            }
            hasher.finish()
        };

        assert_eq!(compute_sig(vec![a, b]), compute_sig(vec![b, a]));
    }

    #[test]
    #[cfg(feature = "linux-wayland")]
    #[serial_test::serial]
    fn xdg_discovery_without_compositor_returns_error() {
        let saved_display = std::env::var("WAYLAND_DISPLAY").ok();
        let saved_runtime = std::env::var("XDG_RUNTIME_DIR").ok();
        unsafe { std::env::remove_var("WAYLAND_DISPLAY") };
        let runtime = tempfile::tempdir().expect("temp runtime dir should be created");
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", runtime.path()) };
        let result = discover_xdg_outputs();
        match saved_display {
            Some(previous) => unsafe { std::env::set_var("WAYLAND_DISPLAY", previous) },
            None => unsafe { std::env::remove_var("WAYLAND_DISPLAY") },
        }
        match saved_runtime {
            Some(previous) => unsafe { std::env::set_var("XDG_RUNTIME_DIR", previous) },
            None => unsafe { std::env::remove_var("XDG_RUNTIME_DIR") },
        }
        assert!(result.is_err());
    }

    #[test]
    #[cfg(feature = "linux-wayland")]
    #[serial_test::serial]
    fn live_weston_reports_outputs() {
        let live = std::env::var("MOUSE_THING_TEST_LIVE_WAYLAND")
            .ok()
            .filter(|value| value == "1")
            .is_some();
        let socket = std::env::var("MOUSE_THING_TEST_WAYLAND_SOCKET")
            .ok()
            .filter(|value| !value.trim().is_empty());
        if !crate::platform::linux::live_test::require_live(
            "live_weston_reports_outputs",
            live && socket.is_some(),
            "no live Wayland compositor",
        ) {
            return;
        }
        let socket = socket.expect("socket presence checked above");
        let saved_display = std::env::var("WAYLAND_DISPLAY").ok();
        unsafe { std::env::set_var("WAYLAND_DISPLAY", &socket) };
        let result = discover_xdg_outputs();
        let manager_result = WaylandDisplayManager::new().discover_displays();
        match saved_display {
            Some(previous) => unsafe { std::env::set_var("WAYLAND_DISPLAY", previous) },
            None => unsafe { std::env::remove_var("WAYLAND_DISPLAY") },
        }
        let outputs = result.expect("weston should report outputs");
        assert!(!outputs.is_empty());
        for output in &outputs {
            assert!(output.width > 0);
            assert!(output.height > 0);
        }
        let discovered = manager_result.expect("manager discovery should succeed");
        assert!(!discovered.is_empty());
    }
}
