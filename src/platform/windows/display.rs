//! Windows display topology discovery and coordinate management.
//!
//! This module provides display topology discovery using Win32 GDI APIs.
//! It discovers all active monitors, computes virtual desktop bounds, and
//! provides coordinate clamping for absolute mouse movements.
//!
//! # Display Signature
//!
//! Generates a stable hash signature of the current monitor configuration.
//! This allows the engine to detect when displays are added, removed, or
//! reordered.

use anyhow::{Result, anyhow};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::mem::size_of;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::platform::{DisplayBounds, DisplayManager};

use windows_sys::Win32::Foundation::{BOOL, LPARAM, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN,
};

/// Representation of a monitor's bounding rectangle in virtual coordinates.
#[derive(Clone, Copy)]
pub(crate) struct MonitorRect {
    pub(crate) left: i32,
    pub(crate) top: i32,
    pub(crate) right: i32,
    pub(crate) bottom: i32,
}

impl MonitorRect {
    fn from_rect(rect: RECT) -> Self {
        Self {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        }
    }

    fn width(self) -> u32 {
        (self.right - self.left).max(1) as u32
    }

    fn height(self) -> u32 {
        (self.bottom - self.top).max(1) as u32
    }

    fn contains(self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }

    fn clamp(self, x: i32, y: i32) -> (i32, i32) {
        let max_x = self.right.saturating_sub(1).max(self.left);
        let max_y = self.bottom.saturating_sub(1).max(self.top);
        (x.clamp(self.left, max_x), y.clamp(self.top, max_y))
    }

    fn as_bounds(self) -> DisplayBounds {
        DisplayBounds {
            x: self.left,
            y: self.top,
            width: self.width(),
            height: self.height(),
        }
    }
}

/// Snapshot of all active monitors and virtual desktop bounds.
#[derive(Clone)]
struct DisplayTopology {
    monitors: Vec<MonitorRect>,
    virtual_bounds: MonitorRect,
}

impl DisplayTopology {
    fn discover() -> Result<Self> {
        let mut monitors = enumerate_monitors()?;
        if monitors.is_empty() {
            monitors.push(virtual_rect_from_metrics()?);
        }

        let virtual_bounds =
            virtual_rect_from_metrics().unwrap_or_else(|_| combine_rects(&monitors));
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

/// Windows display manager implementing [`DisplayManager`] using GDI.
pub(crate) struct WindowsDisplayManager;

impl WindowsDisplayManager {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl DisplayManager for WindowsDisplayManager {
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
        let mut point = POINT { x: 0, y: 0 };
        // SAFETY: `point` is a valid stack-allocated POINT struct.
        if unsafe { GetCursorPos(&mut point) } == 0 {
            return Err(anyhow!(
                "GetCursorPos failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok((point.x, point.y))
    }
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
        return MonitorRect {
            left: 0,
            top: 0,
            right: 1,
            bottom: 1,
        };
    }

    MonitorRect {
        left: min_x,
        top: min_y,
        right: max_x,
        bottom: max_y,
    }
}

struct CachedVirtualRect {
    rect: MonitorRect,
    fetched_at: Instant,
}

static VIRTUAL_RECT_CACHE: OnceLock<Mutex<Option<CachedVirtualRect>>> = OnceLock::new();
const VIRTUAL_RECT_TTL: Duration = Duration::from_millis(500);

pub(crate) fn virtual_rect_from_metrics() -> Result<MonitorRect> {
    if let Some(cached) = VIRTUAL_RECT_CACHE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .expect("virtual rect cache lock poisoned")
        .as_ref()
        .filter(|cached| cached.fetched_at.elapsed() < VIRTUAL_RECT_TTL)
    {
        return Ok(cached.rect);
    }
    // SAFETY: GetSystemMetrics is thread-safe and requires no initialization from caller.
    let x = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    let y = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    let width = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
    let height = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };

    if width <= 0 || height <= 0 {
        return Err(anyhow!(
            "GetSystemMetrics returned invalid virtual desktop size"
        ));
    }

    let rect = MonitorRect {
        left: x,
        top: y,
        right: x.saturating_add(width),
        bottom: y.saturating_add(height),
    };
    *VIRTUAL_RECT_CACHE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .expect("virtual rect cache lock poisoned") = Some(CachedVirtualRect {
        rect,
        fetched_at: Instant::now(),
    });
    Ok(rect)
}

fn enumerate_monitors() -> Result<Vec<MonitorRect>> {
    let mut monitors = Vec::<MonitorRect>::new();
    let data_ptr = &mut monitors as *mut Vec<MonitorRect>;

    // SAFETY: Callback and context pointer remain valid for the duration of the call.
    let ok = unsafe {
        EnumDisplayMonitors(
            0 as HDC,
            std::ptr::null(),
            Some(enum_monitor_proc),
            data_ptr as LPARAM,
        )
    };

    if ok == 0 {
        return Err(anyhow!(
            "EnumDisplayMonitors failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    Ok(monitors)
}

unsafe extern "system" fn enum_monitor_proc(
    monitor: HMONITOR,
    _hdc: HDC,
    _clip: *mut RECT,
    data: LPARAM,
) -> BOOL {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: `data` is the same pointer to `Vec<MonitorRect>` provided by `enumerate_monitors`.
        let monitors = unsafe { &mut *(data as *mut Vec<MonitorRect>) };
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            rcMonitor: RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            rcWork: RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            dwFlags: 0,
        };

        // SAFETY: `monitor` is provided by EnumDisplayMonitors; `info` has initialized cbSize.
        if unsafe { GetMonitorInfoW(monitor, &mut info as *mut MONITORINFO) } != 0 {
            monitors.push(MonitorRect::from_rect(info.rcMonitor));
        }

        1
    }));

    result.unwrap_or_else(|err| {
        eprintln!("[windows-display] caught panic in enum_monitor_proc: {:?}", err);
        1
    })
}
