use legato_proto::{Display, Rect, Screens};
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, MDT_EFFECTIVE_DPI,
    SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;
use windows::core::BOOL;

/// Makes all coordinates this process sees physical pixels, on every monitor. Call once at
/// startup, before creating windows or reading monitor geometry. Safe to call again.
pub fn init_dpi_awareness() {
    // SAFETY: no pointers involved. Fails harmlessly if the awareness was already set.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

/// The monitors, in physical pixels of the virtual desktop, ordered left to right (then
/// top to bottom) so that display indices match how they sit on the desk.
pub fn screens() -> Screens {
    init_dpi_awareness();
    let mut monitors: Vec<HMONITOR> = Vec::new();
    // SAFETY: the callback only runs during this call and `monitors` outlives it.
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(collect_monitor),
            LPARAM(&raw mut monitors as isize),
        );
    }
    let mut displays: Vec<Display> = monitors.into_iter().filter_map(display_info).collect();
    displays.sort_by(|a, b| {
        (a.bounds.x, a.bounds.y)
            .partial_cmp(&(b.bounds.x, b.bounds.y))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let native_per_desk = displays
        .iter()
        .find(|d| d.primary)
        .map_or(1.0, |d| d.ui_scale);
    Screens {
        displays,
        native_per_desk,
    }
}

unsafe extern "system" fn collect_monitor(
    monitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> BOOL {
    // SAFETY: `data` is the `Vec` passed by `screens`, alive for the whole enumeration.
    let monitors = unsafe { &mut *(data.0 as *mut Vec<HMONITOR>) };
    monitors.push(monitor);
    BOOL(1)
}

fn display_info(monitor: HMONITOR) -> Option<Display> {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: `info` is a correctly sized MONITORINFOEXW.
    let ok = unsafe { GetMonitorInfoW(monitor, (&raw mut info).cast::<MONITORINFO>()) };
    if !ok.as_bool() {
        return None;
    }
    let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
    // SAFETY: both out-pointers are valid.
    let _ = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
    let r = info.monitorInfo.rcMonitor;
    let device = String::from_utf16_lossy(&info.szDevice)
        .trim_end_matches('\0')
        .to_string();
    // "\\.\DISPLAY2" → 2, stable across runs while the cabling doesn't change.
    let id = device
        .trim_start_matches(r"\\.\DISPLAY")
        .parse()
        .unwrap_or(0);
    Some(Display {
        id,
        bounds: Rect::new(
            r.left as f64,
            r.top as f64,
            (r.right - r.left) as f64,
            (r.bottom - r.top) as f64,
        ),
        pixel_scale: 1.0,
        ui_scale: dpi_x as f64 / 96.0,
        primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
        name: device.trim_start_matches(r"\\.\").to_string(),
    })
}
