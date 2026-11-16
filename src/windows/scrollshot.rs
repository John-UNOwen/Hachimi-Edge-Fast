//! Scrolling screenshot: scroll a screen with real mouse-wheel input, capture the backbuffer at
//! every settled step, and stitch the frames into one tall PNG that also lands on the clipboard.
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
    SendInput, MOUSEEVENTF_WHEEL, MOUSEINPUT, INPUT_0, INPUT_MOUSE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::INPUT;
use windows::Win32::UI::WindowsAndMessaging::{GetClientRect, GetCursorPos, SetCursorPos};

use crate::core::gui::{request_notification, NotificationRequest};
use crate::core::hachimi::recover_lock;
use crate::windows::utils::get_game_dir;
use crate::windows::wnd_hook;

// One mouse-wheel notch (WHEEL_DELTA). Notches per step are a guess at "a visible move"; the
// stitch measures the actual displacement, so a wrong guess costs a step, never a row.
const WHEEL_DELTA: i32 = 120;
const WHEEL_NOTCHES: i32 = 3;

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

// End conditions. A stopped scroll is only believed after STILL_STEPS_AT_END in a row, because one
// dropped wheel event must not end the capture early. MAX_STEPS and MAX_CANVAS_BYTES are the cost
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

/// Screen position of the middle of the scrolling region, so the wheel reaches the widget the
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

fn send_wheel(notches: i32) {
    let input = [INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                // Negative: scroll down. The field is a u32 holding the signed delta.
                mouseData: (notches * WHEEL_DELTA).wrapping_neg() as u32,
                dwFlags: MOUSEEVENTF_WHEEL,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }];
    let sent = unsafe { SendInput(&input, std::mem::size_of::<INPUT>() as i32) };
    if sent != 1 {
        warn!("scrollshot: SendInput reported {sent} of 1 wheel events delivered");
    }
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
                if x < left {
                    left = x;
                }
                if x > right {
                    right = x;
                }
            }
        }
        if changed {
            if top == usize::MAX {
                top = y;
            }
            bottom = y + 1;
        }
    }

    if top == usize::MAX {
        return None;
    }
    // Widen a little: antialiased edges sit just outside the strict diff.
    let left = left.saturating_sub(8);
    let right = (right + 8).min(w);
    Some(Region { top, bottom, left, right })
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
    // First pass stays within the narrow window of the previous step - wheel steps are similar
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
            let _ = unsafe { SetCursorPos(x, y) };
        }

        let first = request_frame().ok_or_else(|| "timeout waiting for game frames".to_owned())?;
        if first.w < 64 || first.h < 64 {
            return Err("unexpected frame size".to_owned());
        }

        send_wheel(WHEEL_NOTCHES);
        let mut last = settle_capture().ok_or_else(|| "timeout waiting for game frames".to_owned())?;
        if last.w != first.w || last.h != first.h {
            return Err("the window changed size mid-capture".to_owned());
        }

        let Some(region) = detect_region(&first, &last) else {
            // The screen did not move on the first wheel: nothing to stitch, one viewport is a
            // perfectly good screenshot.
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

        // Aim the wheel at the middle of what is actually moving, not at the window middle.
        if let Some((x, y)) = region_center(hwnd, &region) {
            let _ = unsafe { SetCursorPos(x, y) };
        }

        let stride = if first.w >= 1920 { 3 } else { 2 };
        let mw = match_width(first.w);
        let probe = (region.height() / 6).clamp(PROBE_ROWS_MIN, PROBE_ROWS_MAX);

        let mut stitcher = Stitcher::new(first.w, region);
        stitcher.add_first(&first);

        let mut prev_gray = gray_of(&last, region.top, region.bottom, 0, mw, stride);
        let mut steps = 1usize;
        let mut skipped = 0usize;
        let mut still = 0usize;
        let mut last_moved: Option<usize> = None;

        while steps < MAX_STEPS {
            if CANCEL.load(Ordering::Relaxed) {
                info!("scrollshot: cancelled by the player after {steps} steps");
                break;
            }
            send_wheel(WHEEL_NOTCHES);
            let Some(cur) = settle_capture() else {
                return Err("timeout waiting for game frames".to_owned());
            };
            if cur.w != first.w || cur.h != first.h {
                return Err("the window changed size mid-capture".to_owned());
            }
            steps += 1;

            let cur_gray = gray_of(&cur, region.top, region.bottom, 0, mw, stride);
            match travel(&prev_gray, &cur_gray, probe, last_moved) {
                None => {
                    // Cannot place it: add it whole and keep counting it.
                    skipped += 1;
                    if !stitcher.add_whole(&cur) {
                        break;
                    }
                }
                Some(m) if m < STILL_PX => {
                    still += 1;
                    if still >= STILL_STEPS_AT_END {
                        info!("scrollshot: reached the bottom after {steps} steps");
                        break;
                    }
                }
                Some(m) => {
                    still = 0;
                    last_moved = Some(m);
                    match stitcher.add_more(&cur, m) {
                        None => {
                            // `travel` said rows moved yet none were new: treat it as still.
                            still += 1;
                            if still >= STILL_STEPS_AT_END {
                                break;
                            }
                        }
                        Some(false) => break, // canvas cap reached
                        Some(true) => {}
                    }
                }
            }
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
    fn travel_recovers_every_scroll_offset_exactly() {
        let scene = synth();
        let mut pos = 0usize;
        let region = detect_region(&viewport(&scene, 0), &viewport(&scene, 137)).unwrap();
        let mut prev_g = gray_of(&viewport(&scene, 0), region.top, region.bottom, 0, match_width(W), 2);
        let mut last_moved = None;
        // Irregular offsets prove the shift is measured, not assumed from the wheel size.
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
}
