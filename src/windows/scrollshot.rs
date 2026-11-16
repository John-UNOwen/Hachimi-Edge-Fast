//! Scrolling screenshot: scroll a screen with real mouse input, capture the backbuffer at every
//! settled step, and stitch the frames into one tall PNG that also lands on the clipboard.
//!
//! The wheel leads and the drag is the fallback, decided per session by a probe. The wheel rides
//! the same pointer position as everything else: the pointer is parked over the aim point with a
//! real `SendInput` move, then the notches go in, so the widget the game raycasts for its scroll
//! handler is the one under the cursor. That distinction is the whole reason a first build failed:
//! it teleported the cursor with `SetCursorPos`, which produces no message, and measured no motion.
//! If the wheel answers nothing at any probe point, only then does the probe press: the whole
//! candidate ladder retries as the drag the lists are known to answer, and the session runs on
//! that. A "moved" probe counts only as a placeable scroll - a dialog closing behind a stray
//! press is a huge diff and no scroll, and gets rejected.
//!
//! The stitching method is the receipt-factor one the Mirako toolchain documents: find where the
//! foot of the previous frame landed in the next one, demand that the *whole* shared stretch agrees
//! (one band can match by chance, the whole overlap cannot), and treat a frame that cannot be
//! placed as new content rather than letting a coincidence eat a row. See
//! `projects/mirako-machine` knowledge and <https://lt900ed.github.io/receipt_factor/>.
//!
//! Frames come from the game's own swap chain: while a session runs, the `Present` detour copies
//! the backbuffer into a staging texture and hands the RGBA bytes over a slot. The worker thread
//! owns all timing; the present body does at most one small copy and only when asked.

use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rust_i18n::t;
use windows::core::Interface;
use windows::Win32::Foundation::{HANDLE, HWND, POINT, RECT};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ, D3D11_MAP_READ,
    D3D11_MAP_FLAG_DO_NOT_WAIT, D3D11_MAPPED_SUBRESOURCE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
use windows::Win32::Graphics::Dxgi::IDXGISwapChain;
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_DIB;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_WHEEL, MOUSE_EVENT_FLAGS, MOUSEINPUT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetCursorPos, GetSystemMetrics, SetCursorPos, SM_CXVIRTUALSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use crate::core::gui::{request_notification, NotificationRequest};
use crate::core::hachimi::recover_lock;
use crate::windows::utils::get_game_dir;
use crate::windows::wnd_hook;

// The drag. The press rests a beat before moving so the game reads a grab and not a flick, and the
// move lands in hops a frame apart because a ScrollRect drag is built from the pointer's per-frame
// positions. The release is held still before the button lifts for the opposite reason: the game
// estimates fling velocity from the last hops, and a still beat before release means the list
// arrives and stops instead of flying - a release straight off a fast hop was the way a first drag
// flung the list while the capture thread chased it and later presses killed its momentum.
const DRAG_STEPS: usize = 8;
const DRAG_STEP_WAIT: Duration = Duration::from_millis(50);
const DRAG_PRESS_WAIT: Duration = Duration::from_millis(120);
const DRAG_RELEASE_HOLD: Duration = Duration::from_millis(120);
const DRAG_MIN_STEP: usize = 40;
// The wheel step. Notches are the only size the wheel takes, so the count adapts to what the
// previous step measured (see `wheel_notches_for`); the cap keeps a misread scale from turning one
// step into a viewport and skipping past what `travel` can place.
const WHEEL_PROBE_NOTCHES: usize = 2;
const WHEEL_MIN_NOTCHES: usize = 1;
const WHEEL_MAX_NOTCHES: usize = 6;
const WHEEL_ARM_WAIT: Duration = Duration::from_millis(150);
const WHEEL_STEP_WAIT: Duration = Duration::from_millis(60);
const WHEEL_REST_HOLD: Duration = Duration::from_millis(200);
// The cap on one step: the whole gesture plus the settle has to finish before the next press with
// the list at rest, so the step stays a distance the game can lay down inside one settle.
const DRAG_STEP_CAP: usize = 500;
// A step the stitch cannot place is a duplicated viewport, and `travel` places one only while a
// full probe band survives the shift, so a step stays this many rows short of the region height.
const DRAG_STEP_MARGIN: usize = PROBE_ROWS_MAX + 20;

// Timing. MENU_FADE_WAIT lets the menu finish its closing animation before the first frame,
// SETTLE_POLL is the gap between two frames compared for "the scroll came to rest".
const MENU_FADE_WAIT: Duration = Duration::from_millis(600);
const SETTLE_POLL: Duration = Duration::from_millis(140);
const SETTLE_MAX_POLLS: usize = 14;
const FRAME_WAIT_TIMEOUT: Duration = Duration::from_millis(4000);

// Matching, in grayscale levels and pixels. A pair of frames is "still" under SETTLE_MAD; a band
// match needs MATCH_MIN normalized correlation and the whole shared stretch then has to agree
// within OVERLAP_MAX_MAD mean absolute difference (the receipt-factor second opinion).
const SETTLE_MAD: f64 = 1.0;
const MATCH_MIN: f64 = 0.90;
const MATCH_BREAK: f64 = 0.985;
const OVERLAP_MAX_MAD: f64 = 6.0;
const BAND_MIN_STD: f64 = 12.0;
const PROBE_ROWS_MIN: usize = 30;
const PROBE_ROWS_MAX: usize = 110;
const BAND_CANDIDATES: usize = 8;
const NARROW_WINDOW: usize = 48;
const STILL_PX: usize = 3;
// A scrolled list rewrites a wide, tall stretch of the viewport between two frames. A blinking
// readout - the FPS counter proved it live - differs in only a few thin rows, and a probe that
// believed it starts a session aimed at a number ticker. Below these extents a diff is chrome.
const MIN_MOVED_ROWS: usize = 40;
const MIN_MOVED_COLS: usize = 120;
const MIN_MOVED_PIXELS: usize = 6_000;

// End conditions. A stopped scroll is only believed after STILL_STEPS_AT_END in a row, because one
// dropped drag must not end the capture early. MAX_STEPS and MAX_CANVAS_BYTES are the cost
// guards: 60 viewports of anything, and the stitched image is capped at 64 MB of raw pixels.
const STILL_STEPS_AT_END: usize = 3;
const MAX_STEPS: usize = 60;
const MAX_CANVAS_BYTES: usize = 64 * 1024 * 1024;

/// A captured client-area frame, RGBA bytes top-down, tightly packed (`w * 4` per row).
#[derive(Clone)]
pub struct Frame {
    pub w: usize,
    pub h: usize,
    pub rgba: Vec<u8>,
}

impl Frame {
    #[inline]
    fn row_bytes(&self) -> usize {
        self.w * 4
    }
}

/// The part of the screen that actually moved between two frames: `[top, bottom)` rows,
/// `[left, right)` columns. Rows outside are fixed chrome and are kept once in the output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub top: usize,
    pub bottom: usize,
    pub left: usize,
    pub right: usize,
}

impl Region {
    pub fn height(&self) -> usize {
        self.bottom - self.top
    }
}

/// Row-major grayscale, `cw` samples per row (columns already subsampled and cropped).
pub struct Gray {
    pub cw: usize,
    pub rows: usize,
    pub data: Vec<u8>,
}

// ---------------------------------------------------------------------------
// Session state. The present thread only ever touches these when `WANT` is set,
// which only a live session ever sets: idle cost is one relaxed load per frame.
// ---------------------------------------------------------------------------

static ACTIVE: AtomicBool = AtomicBool::new(false);
static CANCEL: AtomicBool = AtomicBool::new(false);
static WANT: AtomicBool = AtomicBool::new(false);
/// A `CopyResource` into the staging texture is in flight and still needs to be mapped.
static STAGED: AtomicBool = AtomicBool::new(false);
static SEQ: AtomicU64 = AtomicU64::new(0);
static SLOT: Mutex<Option<Frame>> = Mutex::new(None);

struct Staging {
    tex: ID3D11Texture2D,
    device: ID3D11Device,
    w: u32,
    h: u32,
}

/// Only the present thread locks this, so it is never contended; the mutex is just the holder.
static STAGING: Mutex<Option<Staging>> = Mutex::new(None);

/// True while a scrolling screenshot session is running.
pub fn is_active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// Ask a running session to stop at the next step boundary. The frames already stitched are kept
/// and saved; a cancel is a "that's enough", not an abort.
pub fn cancel() {
    CANCEL.store(true, Ordering::Relaxed);
}

/// Start a session. Called from the menu button, which closes the menu first: the overlay draws
/// into the same swap chain we capture, so the menu has to be gone before the first frame.
pub fn start() {
    if ACTIVE
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        request_notification(NotificationRequest::Custom(t!("notification.scroll_screenshot_busy").to_string()));
        return;
    }
    CANCEL.store(false, Ordering::Relaxed);
    WANT.store(false, Ordering::Relaxed);
    STAGED.store(false, Ordering::Relaxed);

    let spawned = std::thread::Builder::new()
        .name("hachimi-scrollshot".to_owned())
        .spawn(worker);
    if spawned.is_err() {
        ACTIVE.store(false, Ordering::SeqCst);
        request_notification(NotificationRequest::Custom(
            t!("notification.scroll_screenshot_failed", reason = "thread").to_string(),
        ));
    }
}

/// Called from the `Present` detour on the game's render thread. Cheap no-op between sessions.
pub fn capture_if_requested(swap_chain: *mut c_void) {
    if !WANT.load(Ordering::Relaxed) {
        return;
    }
    let _ = capture_now(swap_chain);
}

fn capture_now(swap_chain: *mut c_void) -> Result<(), ()> {
    // `from_raw` assumes ownership of a reference; `ManuallyDrop` gives it back at the end, the
    // same dance `check_hwnd` does: this swap chain belongs to the game.
    let sc = unsafe { ManuallyDrop::new(IDXGISwapChain::from_raw(swap_chain)) };
    let back = unsafe { sc.GetBuffer::<ID3D11Texture2D>(0) }.map_err(|_| ())?;
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { back.GetDesc(&mut desc) };
    if desc.Width == 0 || desc.Height == 0 {
        return Err(());
    }

    let mut guard = recover_lock(&STAGING);
    let stale = match guard.as_ref() {
        Some(s) => s.w != desc.Width || s.h != desc.Height,
        None => true,
    };
    if stale {
        let device: ID3D11Device = unsafe { sc.GetDevice() }.map_err(|_| ())?;
        let staging_desc = D3D11_TEXTURE2D_DESC {
            Width: desc.Width,
            Height: desc.Height,
            MipLevels: 1,
            ArraySize: 1,
            Format: desc.Format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut tex: Option<ID3D11Texture2D> = None;
        unsafe { device.CreateTexture2D(&staging_desc, None, Some(&mut tex)) }.map_err(|_| ())?;
        let tex = tex.ok_or(())?;
        *guard = Some(Staging { tex, device, w: desc.Width, h: desc.Height });
    }
    let staging = guard.as_mut().ok_or(())?;

    if !STAGED.load(Ordering::Acquire) {
        // Hand the copy to the GPU; map it on a later present. Mapping straight after the copy
        // would either stall the render thread or fail, every single frame.
        let context: ID3D11DeviceContext = unsafe { staging.device.GetImmediateContext() }.map_err(|_| ())?;
        unsafe { context.CopyResource(&staging.tex, &back) };
        STAGED.store(true, Ordering::Release);
        return Ok(());
    }

    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    let mapped_result = unsafe {
        let context = staging.device.GetImmediateContext().map_err(|_| ())?;
        context.Map(
            &staging.tex,
            0,
            D3D11_MAP_READ,
            D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32,
            Some(&mut mapped),
        )
        .map_err(|_| ())?;
        let w = staging.w as usize;
        let h = staging.h as usize;
        let pitch = mapped.RowPitch as usize;
        let mut rgba = vec![0u8; w * h * 4];
        let src = mapped.pData as *const u8;
        for y in 0..h {
            let row = std::slice::from_raw_parts(src.add(y * pitch), w * 4);
            rgba[y * w * 4..(y + 1) * w * 4].copy_from_slice(row);
        }
        context.Unmap(&staging.tex, 0);
        rgba
    };
    let rgba = mapped_result;

    // Close this request before publishing so a request that arrives mid-publish cannot be
    // answered by the frame it did not ask for and then silently dropped.
    WANT.store(false, Ordering::Release);
    *recover_lock(&SLOT) = Some(Frame { w: staging.w as usize, h: staging.h as usize, rgba });
    SEQ.fetch_add(1, Ordering::Release);
    STAGED.store(false, Ordering::Release);
    Ok(())
}

/// Wait for the present thread to drop a fresh frame into the slot.
fn request_frame() -> Option<Frame> {
    STAGED.store(false, Ordering::SeqCst);
    let target = SEQ.load(Ordering::Relaxed) + 1;
    WANT.store(true, Ordering::SeqCst);

    let deadline = Instant::now() + FRAME_WAIT_TIMEOUT;
    while Instant::now() < deadline {
        if SEQ.load(Ordering::Acquire) >= target {
            if let Some(frame) = recover_lock(&SLOT).take() {
                return Some(frame);
            }
        }
        std::thread::sleep(Duration::from_millis(6));
    }
    WANT.store(false, Ordering::SeqCst);
    None
}

// ---------------------------------------------------------------------------
// Input: park the cursor over the content and let the game scroll itself.
// ---------------------------------------------------------------------------

fn cursor_pos() -> Option<POINT> {
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p) }.ok()?;
    Some(p)
}

fn client_center(hwnd: HWND) -> Option<(i32, i32)> {
    let mut rect = RECT::default();
    unsafe { GetClientRect(hwnd, &mut rect) }.ok()?;
    let mut tl = POINT { x: rect.left, y: rect.top };
    let mut br = POINT { x: rect.right, y: rect.bottom };
    if unsafe { ClientToScreen(hwnd, &mut tl) }.as_bool() && unsafe { ClientToScreen(hwnd, &mut br) }.as_bool() {
        Some(((tl.x + br.x) / 2, (tl.y + br.y) / 2))
    } else {
        None
    }
}

/// Screen position of the middle of the scrolling region, so the drag lands on the widget the
/// capture is actually stitching even when the dialog is off-centre.
fn region_center(hwnd: HWND, region: &Region) -> Option<(i32, i32)> {
    let mut rect = RECT::default();
    unsafe { GetClientRect(hwnd, &mut rect) }.ok()?;
    let mut origin = POINT { x: rect.left, y: rect.top };
    if !unsafe { ClientToScreen(hwnd, &mut origin) }.as_bool() {
        return None;
    }
    let x = origin.x + ((region.left + region.right) / 2) as i32;
    let y = origin.y + ((region.top + region.bottom) / 2) as i32;
    Some((x, y))
}

/// Screen position of a point given in frame pixels. The capture is the client area, so a frame
/// coordinate is the client origin plus the offset - and only screen coordinates reach `SendInput`.
fn client_point_on_screen(hwnd: HWND, fx: i32, fy: i32) -> Option<(i32, i32)> {
    let mut rect = RECT::default();
    unsafe { GetClientRect(hwnd, &mut rect) }.ok()?;
    let mut origin = POINT { x: rect.left, y: rect.top };
    if !unsafe { ClientToScreen(hwnd, &mut origin) }.as_bool() {
        return None;
    }
    Some((origin.x + fx, origin.y + fy))
}

/// Where the probe looks for a scrollable region, in frame pixels, tried in order. The window
/// middle first: the one guess that suits a full-viewport list, which is where the drag era's
/// sessions lived. Dialog content boxes sit elsewhere - a details dialog hugs the left, its list
/// below the middle - so the ladder follows with points inside those boxes. Frame fractions keep
/// every point valid at any resolution; it is still the pixel diff, never a template, that decides
/// what actually moved.
fn probe_points(w: usize, h: usize) -> Vec<(i32, i32)> {
    [
        (0.50f32, 0.50f32),
        (0.30f32, 0.65f32),
        (0.70f32, 0.65f32),
        (0.50f32, 0.35f32),
    ]
    .into_iter()
    .map(|(fx, fy)| ((w as f32 * fx) as i32, (h as f32 * fy) as i32))
    .collect()
}

fn virtual_screen() -> (i32, i32, i32, i32) {
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

fn send_mouse(x: i32, y: i32, flags: MOUSE_EVENT_FLAGS, data: u32) {
    let input = [INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT { dx: x, dy: y, mouseData: data, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    }];
    let sent = unsafe { SendInput(&input, std::mem::size_of::<INPUT>() as i32) };
    if sent != 1 {
        warn!("scrollshot: SendInput reported {sent} of 1 mouse events delivered");
    }
}

/// Move the pointer with a real `WM_MOUSEMOVE`, normalised over the virtual screen the way
/// `MOUSEEVENTF_ABSOLUTE` asks for. This is also how the cursor is *parked*: a teleported cursor is
/// invisible to the game's input system, which is the exact way a first build lost every scroll.
fn send_move_abs(x: i32, y: i32) {
    let (vx, vy, vw, vh) = virtual_screen();

    if vw < 2 || vh < 2 {
        warn!("scrollshot: no virtual screen metrics, the pointer stays where it is");
        return;
    }

    let nx = (((x - vx) as f64 * 65535.0) / (vw as f64 - 1.0)).clamp(0.0, 65535.0).round() as i32;
    let ny = (((y - vy) as f64 * 65535.0) / (vh as f64 - 1.0)).clamp(0.0, 65535.0).round() as i32;
    send_mouse(nx, ny, MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE, 0);
}

/// The y positions of a press-to-release drag, in `steps` hops, ending exactly on `to`.
fn drag_offsets(from: i32, to: i32, steps: usize) -> Vec<i32> {
    (1..=steps).map(|k| from + (to - from) * k as i32 / steps as i32).collect()
}

/// Press at `distance / 2` above the aim, walk `distance` pixels up in per-frame hops, release.
/// The gesture is centred on the aim so it stays inside the moving band, and a pointer that has
/// travelled this far never fires the button it started on: the game culls that as a drag, exactly
/// as it does under a finger.
fn drag_scroll(aim_x: i32, aim_y: i32, distance: usize) {
    let from = aim_y + distance as i32 / 2;
    let to = from - distance as i32;

    send_move_abs(aim_x, from);
    std::thread::sleep(DRAG_PRESS_WAIT);
    send_mouse(aim_x, from, MOUSEEVENTF_LEFTDOWN, 0);
    std::thread::sleep(DRAG_STEP_WAIT);

    for ny in drag_offsets(from, to, DRAG_STEPS) {
        send_move_abs(aim_x, ny);
        std::thread::sleep(DRAG_STEP_WAIT);
    }

    // The pointer stops dead while the button is still down: the game reads the list as arriving,
    // not as thrown, and lets it come to rest before the lift.
    std::thread::sleep(DRAG_RELEASE_HOLD);
    send_mouse(aim_x, to, MOUSEEVENTF_LEFTUP, 0);
}

/// The wheel step: park the pointer over the aim with a real move message, then push `notches`
/// wheel notches down. A wheel event carries the pointer position with it, so the widget the game
/// raycasts for its scroll handler is the one under the aim. Negative delta advances the list
/// downward, the same direction an upward drag takes it. Like the drag, the step ends with a beat
/// of rest: a wheeled ScrollRect glides on inertia too, and the capture wants it arrived.
fn wheel_scroll(aim_x: i32, aim_y: i32, notches: usize) {
    send_move_abs(aim_x, aim_y);
    std::thread::sleep(WHEEL_ARM_WAIT);

    for _ in 0..notches.max(1) {
        send_mouse(aim_x, aim_y, MOUSEEVENTF_WHEEL, (-120i32) as u32);
        std::thread::sleep(WHEEL_STEP_WAIT);
    }

    std::thread::sleep(WHEEL_REST_HOLD);
}

/// How many notches the next wheel step should push so its measured move lands near `target` px,
/// given the last step moved `notch_px` per notch. The wheel's size per notch is nobody's promise,
/// so the count is only ever steered toward the target, and bounded so one misread cannot overrun
/// the overlap `travel` needs.
fn wheel_notches_for(target: usize, notch_px: f64) -> usize {
    if notch_px <= 1.0 {
        return WHEEL_MAX_NOTCHES;
    }

    ((target as f64 / notch_px).round() as i64).clamp(WHEEL_MIN_NOTCHES as i64, WHEEL_MAX_NOTCHES as i64) as usize
}

/// Rows one drag step should scroll: 40% of the band, never so fast the settle loses the list,
/// never past what `travel` can place, and never so small a step costs more settle than content.
fn step_distance(region_height: usize) -> usize {
    let wanted = ((region_height * 40 / 100).max(DRAG_MIN_STEP)).min(DRAG_STEP_CAP);
    let limit = region_height.saturating_sub(DRAG_STEP_MARGIN).max(DRAG_MIN_STEP);
    wanted.min(limit)
}

// ---------------------------------------------------------------------------
// Grayscale and region detection
// ---------------------------------------------------------------------------

#[inline]
fn luma(px: &[u8]) -> u8 {
    // (77, 150, 29) sums to 256: integer Rec.601-ish luma, no float, no rounding drift.
    ((px[0] as u32 * 77 + px[1] as u32 * 150 + px[2] as u32 * 29) >> 8) as u8
}

/// Columns skipped at the right edge when matching: that is where a vertical scrollbar thumb
/// travels, and a moving thumb is the one thing guaranteed to match nothing.
fn scrollbar_excl(w: usize) -> usize {
    if w >= 600 { (w / 40).min(64) } else { 0 }
}

fn match_width(w: usize) -> usize {
    (w - scrollbar_excl(w)).max(16)
}

fn gray_of(frame: &Frame, y0: usize, y1: usize, x0: usize, x1: usize, stride: usize) -> Gray {
    let cw = (x1 - x0 + stride - 1) / stride;
    let rows = y1 - y0;
    let mut data = vec![0u8; cw * rows];
    for (gy, y) in (y0..y1).enumerate() {
        let row = &frame.rgba[y * frame.row_bytes()..(y + 1) * frame.row_bytes()];
        let out = &mut data[gy * cw..(gy + 1) * cw];
        for (gx, x) in (x0..x1).step_by(stride).enumerate() {
            out[gx] = luma(&row[x * 4..x * 4 + 4]);
        }
    }
    Gray { cw, rows, data }
}

fn mad(a: &[u8], b: &[u8]) -> f64 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }
    let mut acc = 0u64;
    for i in 0..n {
        acc += (a[i] as i32 - b[i] as i32).unsigned_abs() as u64;
    }
    acc as f64 / n as f64
}

fn whole_mad(a: &Gray, b: &Gray) -> f64 {
    mad(&a.data, &b.data)
}

/// The rows that differ between two frames, and the columns they touch. Fixed chrome (title bars,
/// bottom button bars) never differs and falls outside the region, so it lands in the output once
/// instead of once per viewport. Returns `None` when nothing moved at all.
pub fn detect_region(f0: &Frame, f1: &Frame) -> Option<Region> {
    if f0.w != f1.w || f0.h != f1.h || f0.w < 64 || f0.h < 64 {
        return None;
    }
    let w = f0.w;
    let h = f0.h;
    let mut top = usize::MAX;
    let mut bottom = 0usize;
    let mut left = usize::MAX;
    let mut right = 0usize;
    let mut changed_rows = 0usize;
    let mut changed_pixels = 0usize;

    for y in 0..h {
        let r0 = &f0.rgba[y * f0.row_bytes()..(y + 1) * f0.row_bytes()];
        let r1 = &f1.rgba[y * f1.row_bytes()..(y + 1) * f1.row_bytes()];
        let mut changed = false;
        for x in 0..w {
            let i = x * 4;
            let d = (r0[i] as i32 - r1[i] as i32).abs()
                + (r0[i + 1] as i32 - r1[i + 1] as i32).abs()
                + (r0[i + 2] as i32 - r1[i + 2] as i32).abs();
            if d > 24 {
                changed = true;
                changed_pixels += 1;
                if x < left {
                    left = x;
                }
                if x > right {
                    right = x;
                }
            }
        }
        if changed {
            changed_rows += 1;
            if top == usize::MAX {
                top = y;
            }
            bottom = y + 1;
        }
    }

    if top == usize::MAX {
        return None;
    }
    // Extents under the minimums are a blinking ticker or a pulsing highlight, not a scrolled
    // list: the probe has to answer nothing and move on to its next candidate.
    if changed_rows < MIN_MOVED_ROWS || right - left < MIN_MOVED_COLS || changed_pixels < MIN_MOVED_PIXELS {
        return None;
    }
    // Widen a little: antialiased edges sit just outside the strict diff.
    let left = left.saturating_sub(8);
    let right = (right + 8).min(w);
    Some(Region { top, bottom, left, right })
}

/// The probe has to find a *scroll*, not merely a change: a stray press that closed a modal
/// rewrote half the screen and was mistaken for a moved list. A diff earns belief only when the
/// previous frame's foot band places inside the next one at a measured shift - the same test
/// every later step passes, run here on the probe's own pair.
fn probe_moved(f0: &Frame, f1: &Frame) -> Option<(Region, usize)> {
    let region = detect_region(f0, f1)?;
    let stride = if f0.w >= 1920 { 3 } else { 2 };
    let mw = match_width(f0.w);
    let band = (region.height() / 6).clamp(PROBE_ROWS_MIN, PROBE_ROWS_MAX);
    let g0 = gray_of(f0, region.top, region.bottom, 0, mw, stride);
    let g1 = gray_of(f1, region.top, region.bottom, 0, mw, stride);
    match travel(&g0, &g1, band, None) {
        Some(m) if m >= STILL_PX => Some((region, m)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Travel: how far did the content move up between two settled frames
// ---------------------------------------------------------------------------

fn band_std(g: &Gray, top: usize, probe: usize) -> f64 {
    let mut sum = 0u64;
    let mut count = 0usize;
    for row in (0..probe).step_by(2) {
        let y = (top + row).min(g.rows - 1);
        for &v in &g.data[y * g.cw..(y + 1) * g.cw] {
            sum += v as u64;
            count += 1;
        }
    }
    if count == 0 {
        return 0.0;
    }
    let mean = sum as f64 / count as f64;
    let mut var = 0.0;
    for row in (0..probe).step_by(2) {
        let y = (top + row).min(g.rows - 1);
        for &v in &g.data[y * g.cw..(y + 1) * g.cw] {
            let d = v as f64 - mean;
            var += d * d;
        }
    }
    (var / count as f64).sqrt()
}

/// Zero-mean normalized cross-correlation of `prev[top..top+probe]` against `cur[at..at+probe]`,
/// both full band width. This is `TM_CCOEFF_NORMED` restricted to the vertical offsets that pure
/// scrolling can produce; the search is over `at`, never over x.
fn band_ncc(prev: &Gray, cur: &Gray, top: usize, probe: usize, at: usize) -> f64 {
    if top + probe > prev.rows || at + probe > cur.rows {
        return -1.0;
    }
    let cw = prev.cw;

    let mut psum = 0u64;
    let mut csum = 0u64;
    let n = (probe * cw) as f64;
    for row in 0..probe {
        let pr = &prev.data[(top + row) * cw..(top + row + 1) * cw];
        let cr = &cur.data[(at + row) * cw..(at + row + 1) * cw];
        for k in 0..cw {
            psum += pr[k] as u64;
            csum += cr[k] as u64;
        }
    }
    let pmean = psum as f64 / n;
    let cmean = csum as f64 / n;

    let mut num = 0.0;
    let mut pvar = 0.0;
    let mut cvar = 0.0;
    for row in 0..probe {
        let pr = &prev.data[(top + row) * cw..(top + row + 1) * cw];
        let cr = &cur.data[(at + row) * cw..(at + row + 1) * cw];
        for k in 0..cw {
            let pd = pr[k] as f64 - pmean;
            let cd = cr[k] as f64 - cmean;
            num += pd * cd;
            pvar += pd * pd;
            cvar += cd * cd;
        }
    }
    if pvar < 1.0 || cvar < 1.0 {
        return -1.0;
    }
    num / (pvar * cvar).sqrt()
}

/// The measured displacement in rows, or `None` when the frames cannot be placed against each
/// other. `Some(0)` means "nothing moved". A `None` never means "finished": the caller keeps going.
pub fn travel(prev: &Gray, cur: &Gray, probe: usize, last_moved: Option<usize>) -> Option<usize> {
    if prev.rows != cur.rows || prev.cw != cur.cw {
        return None;
    }
    let rows = prev.rows;
    if whole_mad(prev, cur) < SETTLE_MAD {
        return Some(0);
    }
    let max_moved = rows.saturating_sub(probe + 10);
    if max_moved < 1 {
        return None;
    }

    // Candidate bands from the foot of the previous frame upwards, the textured ones only, the
    // most textured first: a flat band matches anything, a text row matches where it belongs.
    let mut bands: Vec<(usize, f64)> = Vec::new();
    let step = (rows / 40).max(20);
    let mut top = rows - probe;
    while top > rows / 3 {
        let s = band_std(prev, top, probe);
        if s >= BAND_MIN_STD {
            bands.push((top, s));
        }
        top = top.saturating_sub(step);
    }
    bands.sort_by(|a, b| b.1.total_cmp(&a.1));
    bands.truncate(BAND_CANDIDATES);

    let mut best: Option<(f64, usize)> = None;
    // First pass stays within the narrow window of the previous step - drag steps are similar
    // sizes - and a confident hit there skips the full scan entirely.
    let narrow = last_moved.map(|lm| lm.saturating_sub(NARROW_WINDOW)..=(lm.min(max_moved) + NARROW_WINDOW));
    for pass in 0..2 {
        for &(band_top, _) in &bands {
            let limit = max_moved.min(band_top);
            let mut m = 1usize;
            while m <= limit {
                let in_narrow = matches!(&narrow, Some(r) if r.contains(&m));
                let wanted = (pass == 0 && in_narrow) || (pass == 1 && !in_narrow);
                if wanted {
                    let score = band_ncc(prev, cur, band_top, probe, band_top - m);
                    if score > MATCH_BREAK {
                        return place(prev, cur, probe, m);
                    }
                    if best.map_or(true, |(b, _)| score > b) {
                        best = Some((score, m));
                    }
                }
                m += 1;
            }
        }
        if best.map_or(false, |(s, _)| s >= MATCH_MIN) {
            break; // the narrow window already found a plausible answer
        }
    }

    let (_, m) = best?;
    place(prev, cur, probe, m)
}

/// The receipt-factor second opinion: the band said `m`, so *every* shared row has to agree.
fn place(prev: &Gray, cur: &Gray, probe: usize, m: usize) -> Option<usize> {
    if m == 0 {
        return Some(0);
    }
    let shared = prev.rows.checked_sub(m)?;
    if shared < probe {
        return None;
    }
    let cw = prev.cw;
    let overlap_mad = mad(&prev.data[m * cw..], &cur.data[..shared * cw]);
    if overlap_mad <= OVERLAP_MAX_MAD {
        Some(m)
    } else {
        warn!("scrollshot: band match {m} px rejected by whole-overlap check (MAD {overlap_mad:.2})");
        None
    }
}

// ---------------------------------------------------------------------------
// Stitching
// ---------------------------------------------------------------------------

pub struct Stitcher {
    pub w: usize,
    pub canvas: Vec<u8>,
    pub canvas_rows: usize,
    pub max_rows: usize,
    pub region: Region,
    pub truncated: bool,
}

impl Stitcher {
    pub fn new(w: usize, region: Region) -> Self {
        Stitcher {
            w,
            canvas: Vec::new(),
            canvas_rows: 0,
            max_rows: (MAX_CANVAS_BYTES / (w * 4)).max(1),
            region,
            truncated: false,
        }
    }

    fn append_rows(&mut self, frame: &Frame, from: usize, to: usize) -> bool {
        let want = to - from;
        if self.canvas_rows + want > self.max_rows {
            self.truncated = true;
            return false;
        }
        let start = from * self.w * 4;
        let end = to * self.w * 4;
        self.canvas.extend_from_slice(&frame.rgba[start..end]);
        self.canvas_rows += want;
        true
    }

    /// Frame 0 lays down everything from the top of the window down to the bottom of the moving
    /// region: the top chrome once, plus the first viewport of content.
    pub fn add_first(&mut self, frame: &Frame) {
        self.append_rows(frame, 0, self.region.bottom);
    }

    /// Every later frame contributes only its foot inside the moving band: the `moved` newest
    /// content rows. The foot is taken from `[region.top, region.bottom)`, never from the screen
    /// bottom, because rows past `region.bottom` are fixed chrome (a footer bar) that would
    /// otherwise leak in once per step. A `moved` that leaves nothing new is reported by the
    /// `Option`: the caller keeps it as a "still" step rather than pasting anything.
    pub fn add_more(&mut self, frame: &Frame, moved: usize) -> Option<bool> {
        let new_rows = moved.min(self.region.height());
        if new_rows == 0 {
            return None;
        }
        let from = self.region.bottom - new_rows;
        if self.canvas_rows + new_rows > self.max_rows {
            self.truncated = true;
            return Some(false);
        }
        let start = from * self.w * 4;
        let end = self.region.bottom * self.w * 4;
        self.canvas.extend_from_slice(&frame.rgba[start..end]);
        self.canvas_rows += new_rows;
        Some(true)
    }

    /// An unplaceable frame contributes the whole moving region - never dropping content to a
    /// coincidence is worth the risk of a duplicated band.
    pub fn add_whole(&mut self, frame: &Frame) -> bool {
        self.append_rows(frame, self.region.top, self.region.bottom)
    }

    /// The bottom chrome, once, from the last frame in hand.
    pub fn finish(&mut self, frame: &Frame) {
        if self.canvas.is_empty() {
            self.add_first(frame);
            return;
        }
        if self.region.bottom < frame.h {
            let _ = self.append_rows(frame, self.region.bottom, frame.h);
        }
    }
}

// ---------------------------------------------------------------------------
// Output: PNG, file, clipboard
// ---------------------------------------------------------------------------

fn encode_png(w: usize, h: usize, rgba: &[u8]) -> Result<Vec<u8>, String> {
    let mut buf: Vec<u8> = Vec::with_capacity(w * h / 4);
    {
        let mut encoder = png::Encoder::new(&mut buf, w as u32, h as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(rgba).map_err(|e| e.to_string())?;
    }
    Ok(buf)
}

/// A top-down 32 bpp CF_DIB: BITMAPINFOHEADER with negative height, then BGRA rows.
pub fn build_dib(w: usize, h: usize, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(40 + w * h * 4);
    let height = (h as i32).wrapping_neg(); // negative height: top-down rows
    let fields: [u32; 8] = [
        40,
        w as u32,
        height as u32,
        (1u32 << 16) | 32u32, // planes = 1, bpp = 32
        0,                    // BI_RGB
        0,
        0,
        0,
    ];
    for f in fields {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out.extend_from_slice(&[0u8; 8]); // clrUsed, clrImportant
    let mut px = [0u8; 4];
    for chunk in rgba.chunks_exact(4) {
        px[0] = chunk[2]; // B
        px[1] = chunk[1]; // G
        px[2] = chunk[0]; // R
        px[3] = 255;
        out.extend_from_slice(&px);
    }
    out
}

fn copy_dib_to_clipboard(dib: &[u8]) -> bool {
    unsafe {
        if OpenClipboard(Some(HWND::default())).is_err() {
            return false;
        }
        let ok = (|| -> bool {
            if EmptyClipboard().is_err() {
                return false;
            }
            let Ok(hglobal) = GlobalAlloc(GMEM_MOVEABLE, dib.len()) else {
                return false;
            };
            let ptr = GlobalLock(hglobal);
            if ptr.is_null() {
                return false;
            }
            std::ptr::copy_nonoverlapping(dib.as_ptr(), ptr as *mut u8, dib.len());
            let _ = GlobalUnlock(hglobal);
            SetClipboardData(CF_DIB.0 as u32, Some(HANDLE(hglobal.0))).is_ok()
        })();
        let _ = CloseClipboard();
        ok
    }
}

fn save_path() -> PathBuf {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    get_game_dir().join(format!("scrollshot_{stamp}.png"))
}

// ---------------------------------------------------------------------------
// The session
// ---------------------------------------------------------------------------

fn settle_capture() -> Option<Frame> {
    // Keep grabbing until two consecutive frames agree, i.e. the scroll animation came to rest.
    let mut a = request_frame()?;
    let mut tries = 0usize;
    loop {
        std::thread::sleep(SETTLE_POLL);
        let b = request_frame()?;
        let ga = gray_of(&a, 0, a.h, 0, match_width(a.w), 2);
        let gb = gray_of(&b, 0, b.h, 0, match_width(b.w), 2);
        if whole_mad(&ga, &gb) < SETTLE_MAD {
            return Some(b);
        }
        a = b;
        tries += 1;
        if tries >= SETTLE_MAX_POLLS {
            // Still gliding (an inertial list, or a game that never rests the same way twice).
            // Take the last frame and let the stitch measure what it can.
            std::thread::sleep(SETTLE_POLL);
            return request_frame();
        }
    }
}

fn worker() {
    let saved_cursor = cursor_pos();
    let outcome = (|| -> Result<(usize, usize), String> {
        if wnd_hook::window_unfocused() {
            return Err(t!("notification.scroll_screenshot_unfocused").to_string());
        }
        let hwnd = wnd_hook::get_target_hwnd();
        if hwnd.0.is_null() {
            return Err("no game window".to_owned());
        }

        // Let the menu finish closing before the first frame: the overlay draws into the very
        // swap chain being captured.
        std::thread::sleep(MENU_FADE_WAIT);

        let center = client_center(hwnd);
        if let Some((x, y)) = center {
            // A real move: the game's pointer only follows messages, never teleports.
            send_move_abs(x, y);
        }

        let first = request_frame().ok_or_else(|| "timeout waiting for game frames".to_owned())?;
        if first.w < 64 || first.h < 64 {
            return Err("unexpected frame size".to_owned());
        }

        // The probe walks the ladder of points, wheel only. A wheel notch cannot press anything,
        // so the harmless input gets every candidate before any press happens: a first pass that
        // mixed the drag in pressed a modal's backdrop at the middle point and closed the dialog.
        // Only when the wheel answered nothing anywhere does the drag phase press the same points.
        // Either way a candidate counts only as `probe_moved`: a real, placeable scroll.
        let mut use_wheel = false;
        let mut probe_notches = 1usize;
        let mut last = first.clone();
        let mut aim_probe: Option<(i32, i32)> = None;
        let mut probe_rung = 0usize;
        let mut region: Option<Region> = None;

        for wheel_phase in [true, false] {
            if !wheel_phase && region.is_none() {
                info!("scrollshot: the wheel answered nothing at any probe point, trying the drag");
            }
            for (rung, (px, py)) in probe_points(first.w, first.h).into_iter().enumerate() {
                probe_rung = rung + 1;
                let Some((x, y)) = client_point_on_screen(hwnd, px, py) else {
                    continue;
                };
                if wheel_phase {
                    wheel_scroll(x, y, WHEEL_PROBE_NOTCHES);
                } else {
                    // Sized small enough that even a short list keeps enough overlap for the
                    // region pass to place it.
                    drag_scroll(x, y, (first.h / 6).max(DRAG_MIN_STEP));
                }
                let probe = settle_capture().ok_or_else(|| "timeout waiting for game frames".to_owned())?;
                if probe.w != first.w || probe.h != first.h {
                    return Err("the window changed size mid-capture".to_owned());
                }
                if let Some((r, _m)) = probe_moved(&first, &probe) {
                    use_wheel = wheel_phase;
                    probe_notches = WHEEL_PROBE_NOTCHES;
                    aim_probe = Some((x, y));
                    region = Some(r);
                    last = probe;
                    break;
                }
            }
            if region.is_some() {
                break;
            }
        }

        let Some(region) = region else {
            // No ladder point moved the screen: nothing to stitch, one viewport is a perfectly
            // good screenshot.
            let full = Region { top: 0, bottom: first.h, left: 0, right: first.w };
            let mut stitcher = Stitcher::new(first.w, full);
            stitcher.add_first(&first);
            let png = encode_png(first.w, stitcher.canvas_rows, &stitcher.canvas)?;
            let path = save_path();
            std::fs::write(&path, &png).map_err(|e| e.to_string())?;
            let dib = build_dib(first.w, stitcher.canvas_rows, &stitcher.canvas);
            let clipped = copy_dib_to_clipboard(&dib);
            info!("scrollshot: nothing scrolled, saved one viewport to {}", path.display());
            finish_notifications(&path, clipped, 1, 0, false);
            return Ok((1, stitcher.canvas_rows));
        };

        // Aim candidates: the point the probe moved the list at, then the moving band's middle.
        // A measured still rotates to the next candidate, so ending the session needs the list to
        // answer no candidate at all, not merely one.
        info!(
            "scrollshot: probe point {probe_rung} moved the list with the {}{}",
            if use_wheel { "wheel" } else { "drag" },
            if use_wheel { "" } else { "; the wheel answered nothing there, every step will drag" }
        );
        let mut aims: Vec<(i32, i32)> = Vec::new();
        if let Some(p) = aim_probe {
            aims.push(p);
        }
        if let Some(rc) = region_center(hwnd, &region) {
            if !aims.contains(&rc) {
                aims.push(rc);
            }
        }
        let mut aim_i = 0usize;

        let stride = if first.w >= 1920 { 3 } else { 2 };
        let mw = match_width(first.w);
        let probe = (region.height() / 6).clamp(PROBE_ROWS_MIN, PROBE_ROWS_MAX);

        let mut stitcher = Stitcher::new(first.w, region);
        stitcher.add_first(&first);

        // The probe already scrolled a band between `first` and `last`, and that band has to
        // be stitched like any other step - measured, never assumed - or the canvas jumps it.
        let mut prev_gray = gray_of(&first, region.top, region.bottom, 0, mw, stride);
        let mut steps = 1usize;
        let mut skipped = 0usize;
        let mut still = 0usize;
        let mut last_moved: Option<usize> = None;

        let step_px = step_distance(region.height());
        // The count of notches the next wheel step pushes; the probe's own move steers the first one.
        let mut notches = WHEEL_PROBE_NOTCHES;

        {
            let cur_gray = gray_of(&last, region.top, region.bottom, 0, mw, stride);
            match travel(&prev_gray, &cur_gray, probe, None) {
                None => {
                    skipped += 1;
                    let _ = stitcher.add_whole(&last);
                },
                Some(m) if m >= STILL_PX => {
                    last_moved = Some(m);
                    if use_wheel {
                        notches = wheel_notches_for(step_px, m as f64 / probe_notches.max(1) as f64);
                    }
                    let _ = stitcher.add_more(&last, m);
                },
                Some(_) => {},
            }
            prev_gray = cur_gray;
        }

        while steps < MAX_STEPS {
            if CANCEL.load(Ordering::Relaxed) {
                info!("scrollshot: cancelled by the player after {steps} steps");
                break;
            }
            if let Some((x, y)) = aims.get(aim_i).copied().or(center) {
                if use_wheel {
                    wheel_scroll(x, y, notches);
                } else {
                    drag_scroll(x, y, step_px);
                }
            }
            let used_notches = notches;
            let Some(cur) = settle_capture() else {
                return Err("timeout waiting for game frames".to_owned());
            };
            if cur.w != first.w || cur.h != first.h {
                return Err("the window changed size mid-capture".to_owned());
            }
            steps += 1;

            let cur_gray = gray_of(&cur, region.top, region.bottom, 0, mw, stride);
            let measured: Option<usize> = match travel(&prev_gray, &cur_gray, probe, last_moved) {
                None => {
                    // Cannot place it: add it whole and keep counting it.
                    skipped += 1;
                    if !stitcher.add_whole(&cur) {
                        last = cur;
                        break;
                    }
                    None
                }
                Some(m) if m < STILL_PX => {
                    still += 1;
                    if still == 1 && aims.len() > 1 {
                        aim_i = (aim_i + 1) % aims.len();
                        info!("scrollshot: step measured still, rotating the step to aim candidate {}", aim_i + 1);
                    }
                    if still >= STILL_STEPS_AT_END {
                        info!("scrollshot: reached the bottom after {steps} steps");
                        last = cur;
                        break;
                    }
                    Some(m)
                }
                Some(m) => {
                    still = 0;
                    last_moved = Some(m);
                    if use_wheel {
                        notches = wheel_notches_for(step_px, m as f64 / used_notches.max(1) as f64);
                    }
                    match stitcher.add_more(&cur, m) {
                        None => {
                            // `travel` said rows moved yet none were new: treat it as still.
                            still += 1;
                            if still >= STILL_STEPS_AT_END {
                                last = cur;
                                break;
                            }
                        }
                        Some(false) => {
                            last = cur;
                            break; // canvas cap reached
                        }
                        Some(true) => {}
                    }
                    Some(m)
                }
            };

            // One line per step: this is the thread's whole view of the game, and a session that
            // ends too early is only diagnosable from what each step actually measured.
            let moved_note = measured.map(|m| m.to_string()).unwrap_or_else(|| "unplaced".to_owned());
            let mode_note = if use_wheel {
                format!("wheel x{used_notches}")
            } else {
                format!("drag {step_px} px")
            };
            info!("scrollshot: step {steps}: moved {moved_note} skipped {skipped} still {still} aim {} {mode_note}", aim_i + 1);

            prev_gray = cur_gray;
            last = cur;
        }

        stitcher.finish(&last);
        let png = encode_png(stitcher.w, stitcher.canvas_rows, &stitcher.canvas)?;
        let path = save_path();
        std::fs::write(&path, &png).map_err(|e| e.to_string())?;
        let dib = build_dib(stitcher.w, stitcher.canvas_rows, &stitcher.canvas);
        let clipped = copy_dib_to_clipboard(&dib);
        info!(
            "scrollshot: {steps} steps, {skipped} unplaceable, {}x{} px saved to {}",
            stitcher.w,
            stitcher.canvas_rows,
            path.display()
        );
        finish_notifications(&path, clipped, steps, skipped, stitcher.truncated);
        Ok((steps, stitcher.canvas_rows))
    })();

    restore_cursor(saved_cursor);
    if let Err(reason) = outcome {
        error!("scrollshot: {reason}");
        request_notification(NotificationRequest::Custom(
            t!("notification.scroll_screenshot_failed", reason = reason).to_string(),
        ));
    }
    ACTIVE.store(false, Ordering::SeqCst);
    WANT.store(false, Ordering::SeqCst);
    *recover_lock(&SLOT) = None;
    *recover_lock(&STAGING) = None;
}

fn restore_cursor(saved: Option<POINT>) {
    if let Some(p) = saved {
        let _ = unsafe { SetCursorPos(p.x, p.y) };
    }
}

fn finish_notifications(path: &Path, clipped: bool, steps: usize, skipped: usize, truncated: bool) {
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let mut msg = t!("notification.scroll_screenshot_saved", name = name).to_string();
    if !clipped {
        msg.push(' ');
        msg.push_str(&t!("notification.scroll_screenshot_clip_failed"));
    }
    request_notification(NotificationRequest::Custom(msg));
    if truncated {
        request_notification(NotificationRequest::Custom(
            t!("notification.scroll_screenshot_truncated", steps = steps).to_string(),
        ));
    }
    if skipped > 0 {
        warn!("scrollshot: {skipped} frame(s) could not be placed against their neighbour and were appended whole");
    }
}

// ---------------------------------------------------------------------------
// Tests: synthetic scrolling scenes, no game required
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    struct Scene {
        h: usize,
        rgba: Vec<u8>,
    }

    /// Deterministic pseudo-random content so matching has real texture to find.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u32 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (self.0 >> 33) as u32
        }
    }

    const HEADER: usize = 40;
    const FOOTER: usize = 30;
    const VIEW_H: usize = 300;
    const W: usize = 220;

    /// A tall scene of text-ish noise over a slow gradient (never periodic). Header and footer are
    /// not in the scene: they are screen-fixed chrome pasted over every viewport at the same rows.
    fn synth() -> Scene {
        let total_h = VIEW_H + 900;
        let mut rng = Lcg(0x5eed_1234_9abc_7777);
        let mut rgba = vec![0u8; W * total_h * 4];
        for y in 0..total_h {
            for x in 0..W {
                let i = (y * W + x) * 4;
                let noise = (rng.next() % 7) as i32 * 9; // spiky, non-periodic
                let base = ((x * 3 + y * 11) % 200) as i32;
                let v = (base + noise).clamp(0, 255) as u8;
                rgba[i] = v;
                rgba[i + 1] = v.rotate_left(3);
                rgba[i + 2] = 255u8.wrapping_sub(v);
                rgba[i + 3] = 255;
            }
        }
        Scene { h: total_h, rgba }
    }

    fn viewport(scene: &Scene, y0: usize) -> Frame {
        let mut rgba = Vec::with_capacity(W * VIEW_H * 4);
        for row in 0..VIEW_H {
            let sy = (y0 + row).min(scene.h - 1);
            rgba.extend_from_slice(&scene.rgba[sy * W * 4..(sy + 1) * W * 4]);
        }
        for row in 0..HEADER {
            for x in 0..W {
                let i = (row * W + x) * 4;
                rgba[i] = ((x * 13 + 7) % 255) as u8;
                rgba[i + 1] = ((x * 5 + row) % 255) as u8;
                rgba[i + 2] = 42;
                rgba[i + 3] = 255;
            }
        }
        for row in (VIEW_H - FOOTER)..VIEW_H {
            for x in 0..W {
                let i = (row * W + x) * 4;
                rgba[i] = 200;
                // Row coefficients 29 and 101 are coprime with 255, so no two footer rows are
                // ever equal - the "appears exactly once" tests need that property of the chrome.
                rgba[i + 1] = ((x * 13 + row * 29) % 255) as u8;
                rgba[i + 2] = ((x * 7 + row * 101) % 255) as u8;
                rgba[i + 3] = 255;
            }
        }
        Frame { w: W, h: VIEW_H, rgba }
    }

    #[test]
    fn region_detection_finds_the_moving_band() {
        let scene = synth();
        let f0 = viewport(&scene, 0);
        let f1 = viewport(&scene, 137);
        let region = detect_region(&f0, &f1).expect("content moved");
        assert_eq!(region.top, HEADER, "header rows must read as chrome");
        assert_eq!(region.bottom, VIEW_H - FOOTER, "footer rows must read as chrome");
    }

    #[test]
    fn a_blinking_readout_is_not_a_scrolled_list() {
        // The failure this guard was written from: an FPS ticker in the corner differs between
        // every two frames. Thin across rows and narrow across columns alike, it must read as
        // nothing moved, or the probe declares a win aimed at the counter.
        let scene = synth();
        let f0 = viewport(&scene, 0);

        let mut band = f0.clone();
        for y in 2..14 {
            for x in 0..W {
                let i = (y * W + x) * 4;
                band.rgba[i] = 255;
                band.rgba[i + 1] = 0;
                band.rgba[i + 2] = 255;
            }
        }
        assert!(detect_region(&f0, &band).is_none(), "a few lit rows across the top is chrome");

        let mut strip = f0.clone();
        for y in 2..200 {
            for x in 150..170 {
                let i = (y * W + x) * 4;
                strip.rgba[i] = 255;
                strip.rgba[i + 1] = 0;
                strip.rgba[i + 2] = 255;
            }
        }
        assert!(detect_region(&f0, &strip).is_none(), "a narrow tall strip is chrome");
    }

    #[test]
    fn a_probe_must_be_a_placeable_scroll_not_a_screen_change() {
        let scene = synth();
        let f0 = viewport(&scene, 0);

        let real = viewport(&scene, 137);
        let (_r, m) = probe_moved(&f0, &real).expect("a scrolled list is a real probe");
        assert_eq!(m, 137, "the probe measures the same shift a later step would");

        // A modal closing rewrites the screen with content that never scrolled from anywhere:
        // a big diff, but no foot band that places inside the next frame.
        let mut closed = f0.clone();
        let mut rng = Lcg(0x5ea1_7c0d_e5db_0001);
        for y in 40..(VIEW_H - 40) {
            for x in 0..W {
                let i = (y * W + x) * 4;
                closed.rgba[i] = (rng.next() % 256) as u8;
                closed.rgba[i + 1] = (rng.next() % 256) as u8;
                closed.rgba[i + 2] = (rng.next() % 256) as u8;
            }
        }
        assert!(detect_region(&f0, &closed).is_some(), "the change alone looks huge");
        assert!(probe_moved(&f0, &closed).is_none(), "huge is not scrolled: the probe must reject it");
    }

    #[test]
    fn the_probe_ladder_starts_at_the_middle_and_reaches_dialog_boxes() {
        let pts = probe_points(1920, 1080);
        assert_eq!(pts[0], (960, 540), "a full-viewport list answers at the window middle");
        // The details dialog's skill list spans x 276..826, y 509..912 at this size; one rung
        // must land inside that box or the ladder still only finds full-screen lists.
        assert!(
            pts.iter().skip(1).any(|&(x, y)| x > 276 && x < 826 && y > 509 && y < 912),
            "a rung must reach the skill box of a left-hugging dialog: {pts:?}"
        );
        assert!(pts.iter().all(|&(x, y)| x > 0 && y > 0 && x < 1920 && y < 1080), "every rung stays inside the frame");
    }

    #[test]
    fn travel_recovers_every_scroll_offset_exactly() {
        let scene = synth();
        let mut pos = 0usize;
        let region = detect_region(&viewport(&scene, 0), &viewport(&scene, 137)).unwrap();
        let mut prev_g = gray_of(&viewport(&scene, 0), region.top, region.bottom, 0, match_width(W), 2);
        let mut last_moved = None;
        // Irregular offsets prove the shift is measured, not assumed from the drag size.
        for &off in [137usize, 91, 150, 64, 0].iter() {
            pos += off;
            let cur = viewport(&scene, pos);
            let cur_g = gray_of(&cur, region.top, region.bottom, 0, match_width(W), 2);
            let m = travel(&prev_g, &cur_g, 60, last_moved);
            assert_eq!(m, Some(off), "travel must recover the scroll offset {off}");
            if off > 0 {
                last_moved = Some(off);
            }
            prev_g = cur_g;
        }
    }

    #[test]
    fn identical_frames_measure_zero() {
        let a = viewport(&synth(), 42);
        let b = viewport(&synth(), 42);
        let g1 = gray_of(&a, 0, VIEW_H, 0, match_width(W), 2);
        let g2 = gray_of(&b, 0, VIEW_H, 0, match_width(W), 2);
        assert_eq!(travel(&g1, &g2, 60, None), Some(0));
    }

    #[test]
    fn unrelated_noise_cannot_be_placed() {
        let a = viewport(&synth(), 0);
        let mut rng = Lcg(0xdead_beef_cafe_1234);
        let mut rnd = vec![0u8; W * VIEW_H * 4];
        for b in rnd.iter_mut() {
            *b = (rng.next() % 256) as u8;
        }
        let b = Frame { w: W, h: VIEW_H, rgba: rnd };
        let g1 = gray_of(&a, 0, VIEW_H, 0, match_width(W), 2);
        let g2 = gray_of(&b, 0, VIEW_H, 0, match_width(W), 2);
        assert_eq!(travel(&g1, &g2, 60, None), None, "a coincidence must not be believed");
    }

    #[test]
    fn stitch_rebuilds_the_scene_height_and_pixels() {
        let scene = synth();
        let positions = [0usize, 137, 228, 378, 442];
        let moves = [137usize, 91, 150, 64];
        let frames: Vec<Frame> = positions.iter().map(|&p| viewport(&scene, p)).collect();

        let region = detect_region(&frames[0], &frames[1]).unwrap();
        let mut st = Stitcher::new(W, region);
        st.add_first(&frames[0]);
        let mut expected_rows = region.bottom;
        for k in 1..frames.len() {
            assert_eq!(st.add_more(&frames[k], moves[k - 1]), Some(true));
            expected_rows += moves[k - 1];
        }
        st.finish(&frames[frames.len() - 1]);
        expected_rows += FOOTER;
        assert_eq!(st.canvas_rows, expected_rows);

        // The first viewport's content rows in the canvas must be the scene itself, row for row.
        let mut mismatches = 0usize;
        for j in 0..region.height() {
            let sy = region.top + j;
            let canvas_row = &st.canvas[sy * W * 4..(sy + 1) * W * 4];
            let scene_row = &scene.rgba[sy * W * 4..(sy + 1) * W * 4];
            if canvas_row != scene_row {
                mismatches += 1;
            }
        }
        assert_eq!(mismatches, 0, "the first viewport must be exact scene content");
    }

    #[test]
    fn chrome_appears_exactly_once() {
        let scene = synth();
        let f0 = viewport(&scene, 0);
        let f1 = viewport(&scene, 150);
        let f2 = viewport(&scene, 280);
        let region = detect_region(&f0, &f1).unwrap();
        let mut st = Stitcher::new(W, region);
        st.add_first(&f0);
        assert_eq!(st.add_more(&f1, 150), Some(true));
        assert_eq!(st.add_more(&f2, 130), Some(true));
        st.finish(&f2);

        let header_sig: Vec<u8> = f0.rgba[(HEADER - 1) * W * 4..HEADER * W * 4].to_vec();
        let mut hits = 0usize;
        for row in 0..st.canvas_rows {
            if &st.canvas[row * W * 4..(row + 1) * W * 4] == header_sig.as_slice() {
                hits += 1;
            }
        }
        assert_eq!(hits, 1, "the last header row must appear exactly once in the canvas");

        let footer_sig: Vec<u8> = f0.rgba[(VIEW_H - 1) * W * 4..VIEW_H * W * 4].to_vec();
        let mut footer_hits = 0usize;
        for row in 0..st.canvas_rows {
            if &st.canvas[row * W * 4..(row + 1) * W * 4] == footer_sig.as_slice() {
                footer_hits += 1;
            }
        }
        assert_eq!(footer_hits, 1, "the footer bar must appear exactly once");
    }

    #[test]
    fn dib_header_and_channel_order() {
        let rgba = vec![10u8, 20, 30, 255, 40, 50, 60, 255];
        let dib = build_dib(2, 1, &rgba);
        assert_eq!(dib.len(), 48, "40 header + 2 BGRA pixels");
        assert_eq!(&dib[0..4], &40u32.to_le_bytes());
        assert_eq!(&dib[4..8], &2u32.to_le_bytes());
        assert_eq!(&dib[8..12], &(1i32).wrapping_neg().to_le_bytes(), "top-down DIB");
        assert_eq!(&dib[12..16], &((1u32 << 16) | 32u32).to_le_bytes());
        assert_eq!(&dib[40..44], &[30, 20, 10, 255], "BGRA on the clipboard");
    }

    #[test]
    fn repeated_still_steps_terminate_a_repeating_scene() {
        // A periodic scene is where band matching could believe a wrong shift; whatever travel
        // decides, two identical frames must read as "still" so the session can end.
        let mut base = vec![0u8; W * VIEW_H * 4];
        for y in 0..VIEW_H {
            for x in 0..W {
                let i = (y * W + x) * 4;
                let v = ((x / 4) % 32) as u8 * 8 + (y % 8) as u8;
                base[i] = v;
                base[i + 1] = v;
                base[i + 2] = v;
                base[i + 3] = 255;
            }
        }
        let a = Frame { w: W, h: VIEW_H, rgba: base.clone() };
        let b = Frame { w: W, h: VIEW_H, rgba: base };
        let g1 = gray_of(&a, 0, VIEW_H, 0, match_width(W), 2);
        let g2 = gray_of(&b, 0, VIEW_H, 0, match_width(W), 2);
        assert_eq!(travel(&g1, &g2, 60, None), Some(0), "identical repeats must read as still");
    }

    #[test]
    fn a_drag_path_walks_from_press_to_release() {
        let path = drag_offsets(500, 300, DRAG_STEPS);
        assert_eq!(path.len(), DRAG_STEPS);
        assert_eq!(*path.last().unwrap(), 300, "the last hop lands exactly on the release point");
        assert!(path.windows(2).all(|w| w[1] < w[0]), "every hop moves toward the release");
        // A hop is only a drag if the game sees it inside a frame: the path never stands still.
        assert!(drag_offsets(300, 300, DRAG_STEPS).windows(2).all(|w| w[1] == w[0]));
    }

    #[test]
    fn a_drag_step_never_outruns_what_the_stitch_can_place() {
        // travel() places a step only while a full probe band survives, and the settle has to
        // catch the list at rest between presses, so a step answers to three bounds: the band's
        // share, the fling cap, and the stitch margin - whichever is smallest wins.
        assert_eq!(step_distance(800), 320, "a tall band takes the share");
        assert_eq!(step_distance(1500), DRAG_STEP_CAP, "a window taller than the cap takes the cap");
        assert_eq!(step_distance(200), 200 - DRAG_STEP_MARGIN, "a short band takes the stitch limit");
        assert!(step_distance(120) >= DRAG_MIN_STEP, "a hopeless band keeps a sane floor");
    }

    #[test]
    fn wheel_notches_steer_toward_the_measured_step_and_stay_bounded() {
        // The count follows the px the last step measured per notch, so a 350 px target at 90 px
        // a notch asks for four; a notch that barely moves is chased only to the cap, and one that
        // overruns the target never goes below a single notch.
        assert_eq!(wheel_notches_for(350, 90.0), 4);
        assert_eq!(wheel_notches_for(350, 350.0), 1);
        assert_eq!(wheel_notches_for(350, 2.0), WHEEL_MAX_NOTCHES, "a crawling notch is capped, not chased");
        assert_eq!(wheel_notches_for(50, 400.0), WHEEL_MIN_NOTCHES, "an overrunning notch never goes under one");
        assert_eq!(wheel_notches_for(350, 0.0), WHEEL_MAX_NOTCHES, "an unmeasured notch starts at the cap");
    }
}
