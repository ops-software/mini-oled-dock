//! band_capture.exe (P2-kit S-BAND, K2a.11; dev only, unelevated): starts `gallery --measure band --monitor i --secs n`
//! from its own folder, captures the composed frames of that monitor through DXGI desktop duplication
//! (IDXGIOutput1::DuplicateOutput, IDXGIOutputDuplication::AcquireNextFrame), reads the ruler column of the gallery's
//! window on every frame the desktop presented, and counts the frames whose ruler is not one run of consecutive
//! indices (a tear shows two offsets). It copies only the 16 px column inside the gallery window's client rect, sends no
//! message and touches no window. One JSON line, with the gallery's own line inside; a capture Windows refuses
//! (protected content, the secure desktop, a remote session), or one that read fewer than ten rulers a second (the
//! ruler covered by another window, an output that presented nothing), prints "not measured" and exits 3.

use std::io::Read as _;
use std::os::windows::process::CommandExt as _;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use gallery::measure::band::{classify, decode, Read, RULER_W};
use windows::core::{Interface, BOOL};
use windows::Win32::Foundation::{HMODULE, HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BOX, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_FLAG, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput1, IDXGIResource, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
};
use windows::Win32::Graphics::Gdi::{ClientToScreen, MonitorFromWindow, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::Threading::CREATE_NO_WINDOW;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetClientRect, GetWindowThreadProcessId, IsWindowVisible,
};

/// The gallery's window class.
const CLASS: &str = "MiniOledGallery";
/// How long the gallery's window may take to appear, and to settle (maximised, first paint) after it did.
const APPEAR: Duration = Duration::from_secs(10);
const SETTLE: Duration = Duration::from_millis(1500);

/// `--monitor <i> --secs <n>`, both required.
fn parse(args: &[String]) -> Result<(u32, u32), String> {
    let (mut monitor, mut secs) = (None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let slot = match a.as_str() {
            "--monitor" => &mut monitor,
            "--secs" => &mut secs,
            other => return Err(format!("unknown argument {other:?}")),
        };
        if slot.is_some() {
            return Err(format!("{a} is given twice"));
        }
        let v = it.next().ok_or_else(|| format!("{a} needs a value"))?;
        *slot = Some(v.parse::<u32>().map_err(|_| format!("{a} {v:?}: not a number"))?);
    }
    match (monitor, secs) {
        (Some(m), Some(s)) if s > 0 => Ok((m, s)),
        _ => Err("usage: band_capture --monitor <i> --secs <n>".to_string()),
    }
}

/// The frames a capture saw.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Counts {
    captured: u64,
    clean: u64,
    torn: u64,
    unreadable: u64,
}

impl Counts {
    fn add(&mut self, r: Read) {
        self.captured += 1;
        match r {
            Read::Clean => self.clean += 1,
            Read::Torn { .. } => self.torn += 1,
            Read::Unreadable => self.unreadable += 1,
        }
    }

    /// Torn frames per 100 read ones.
    fn torn_pct(&self) -> f64 {
        let read = self.clean + self.torn;
        if read == 0 {
            0.0
        } else {
            self.torn as f64 * 100.0 / read as f64
        }
    }

    /// Ok when a run of `secs` read enough rulers to say anything: ten a second (a sixth of a 60 Hz output). A covered
    /// ruler or an output that presented nothing reads none, and its torn_pct of 0 would pass with nothing measured.
    fn measured(&self, secs: u32) -> Result<(), String> {
        let (read, need) = (self.clean + self.torn, u64::from(secs) * 10);
        if read < need {
            Err(format!("{read} of {} captured frames had a readable ruler, at least {need} needed", self.captured))
        } else {
            Ok(())
        }
    }
}

/// The JSON line: the monitor, the counts and the gallery's own line (null when it printed none).
fn line(monitor: u32, c: &Counts, gallery: Option<&str>) -> String {
    format!(
        "{{\"monitor\":{monitor},\"captured\":{},\"clean\":{},\"torn\":{},\"unreadable\":{},\"torn_pct\":{:.3},\"gallery\":{}}}",
        c.captured,
        c.clean,
        c.torn,
        c.unreadable,
        c.torn_pct(),
        gallery.unwrap_or("null")
    )
}

/// The visible top-level window of class CLASS that process `pid` owns (EnumWindows, GetClassNameW: no message).
fn window_of(pid: u32) -> Option<HWND> {
    unsafe extern "system" fn each(h: HWND, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the (pid, found) pair window_of passed, alive during the synchronous enumeration.
        let (pid, found) = unsafe { &mut *(data.0 as *mut (u32, Option<HWND>)) };
        let mut owner = 0u32;
        // SAFETY: reading a window's process id and class name sends no message; `name` is writable.
        let mut name = [0u16; 64];
        let n = unsafe {
            GetWindowThreadProcessId(h, Some(&mut owner));
            GetClassNameW(h, &mut name)
        };
        // SAFETY: reading visibility has no precondition.
        let visible = unsafe { IsWindowVisible(h) }.as_bool();
        if owner == *pid && visible && String::from_utf16_lossy(&name[..n.max(0) as usize]) == CLASS {
            *found = Some(h);
            return BOOL(0);
        }
        BOOL(1)
    }
    let mut data: (u32, Option<HWND>) = (pid, None);
    // SAFETY: the callback only touches `data`, which outlives the synchronous enumeration.
    let _ = unsafe { EnumWindows(Some(each), LPARAM(&mut data as *mut (u32, Option<HWND>) as isize)) };
    data.1
}

/// A window's client rect in screen coordinates (GetClientRect and ClientToScreen send no message).
fn client_on_screen(h: HWND) -> Option<RECT> {
    let mut r = RECT::default();
    let mut p = POINT::default();
    // SAFETY: a window of the child process, only read; `r` and `p` are writable.
    unsafe {
        GetClientRect(h, &mut r).ok()?;
        ClientToScreen(h, &mut p).ok().ok()?;
    }
    Some(RECT { left: p.x, top: p.y, right: p.x + r.right, bottom: p.y + r.bottom })
}

/// The duplication of the output showing `h`, a device to copy from, and the output's desktop origin.
struct Capture {
    dup: windows::Win32::Graphics::Dxgi::IDXGIOutputDuplication,
    ctx: ID3D11DeviceContext,
    device: ID3D11Device,
    origin: POINT,
}

fn duplicate(h: HWND) -> Result<Capture, String> {
    // SAFETY: plain COM calls on this thread (COM needs no apartment for DXGI); every out parameter is a local.
    unsafe {
        let mon = MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST);
        let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(|e| format!("CreateDXGIFactory1: {e}"))?;
        let mut a = 0;
        while let Ok(adapter) = factory.EnumAdapters1(a) {
            let mut o = 0;
            while let Ok(output) = adapter.EnumOutputs(o) {
                let desc = output.GetDesc().map_err(|e| format!("GetDesc: {e}"))?;
                if desc.Monitor == mon {
                    let (mut device, mut ctx) = (None, None);
                    D3D11CreateDevice(
                        &adapter,
                        D3D_DRIVER_TYPE_UNKNOWN,
                        HMODULE::default(),
                        D3D11_CREATE_DEVICE_FLAG(0),
                        None,
                        D3D11_SDK_VERSION,
                        Some(&mut device),
                        None,
                        Some(&mut ctx),
                    )
                    .map_err(|e| format!("D3D11CreateDevice: {e}"))?;
                    let (Some(device), Some(ctx)) = (device, ctx) else { return Err("no D3D11 device".to_string()) };
                    let output1: IDXGIOutput1 = output.cast().map_err(|e| format!("IDXGIOutput1: {e}"))?;
                    let dup = output1.DuplicateOutput(&device).map_err(|e| format!("DuplicateOutput: {e}"))?;
                    let c = desc.DesktopCoordinates;
                    return Ok(Capture { dup, ctx, device, origin: POINT { x: c.left, y: c.top } });
                }
                o += 1;
            }
            a += 1;
        }
    }
    Err("no output shows the gallery's monitor".to_string())
}

/// Captures until the gallery exits (or its run plus 5 s): every presented frame's ruler column inside `client` is
/// read and classified.
fn capture(cap: &Capture, client: RECT, secs: u32, child: &mut Child) -> Result<Counts, String> {
    let (w, h) = (RULER_W as u32, (client.bottom - client.top).max(0) as u32);
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };
    let mut staging: Option<ID3D11Texture2D> = None;
    // SAFETY: `desc` lives for the call; the texture is written to a local.
    unsafe { cap.device.CreateTexture2D(&desc, None, Some(&mut staging)) }.map_err(|e| format!("staging: {e}"))?;
    let staging = staging.ok_or("no staging texture")?;
    let (x, y) = ((client.left - cap.origin.x).max(0) as u32, (client.top - cap.origin.y).max(0) as u32);
    let region = D3D11_BOX { left: x, top: y, front: 0, right: x + w, bottom: y + h, back: 1 };
    let end = Instant::now() + Duration::from_secs(u64::from(secs) + 5);
    let mut counts = Counts::default();
    let mut rows = Vec::with_capacity(h as usize);
    while Instant::now() < end && child.try_wait().map(|s| s.is_none()).unwrap_or(false) {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut res: Option<IDXGIResource> = None;
        // SAFETY: both out parameters are locals; the frame is released below before the next one is acquired.
        match unsafe { cap.dup.AcquireNextFrame(100, &mut info, &mut res) } {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => continue,
            Err(e) => return Err(format!("AcquireNextFrame: {e}")),
        }
        if info.LastPresentTime != 0 {
            if let Some(tex) = res.and_then(|r| r.cast::<ID3D11Texture2D>().ok()) {
                rows.clear();
                // SAFETY: the region lies inside the desktop texture (the client rect is on this output); the staging
                // texture is mapped for reading, read within RowPitch x Height, and unmapped.
                unsafe {
                    cap.ctx.CopySubresourceRegion(&staging, 0, 0, 0, 0, &tex, 0, Some(&region));
                    let mut m = D3D11_MAPPED_SUBRESOURCE::default();
                    if cap.ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut m)).is_ok() {
                        for row in 0..h as usize {
                            let p = (m.pData as *const u8).add(row * m.RowPitch as usize) as *const u32;
                            let px = std::slice::from_raw_parts(p, RULER_W);
                            rows.push(decode(px));
                        }
                        cap.ctx.Unmap(&staging, 0);
                        counts.add(classify(&rows));
                    }
                }
            }
        }
        // SAFETY: the frame acquired above.
        let _ = unsafe { cap.dup.ReleaseFrame() };
    }
    Ok(counts)
}

/// Runs the capture of monitor `monitor` for `secs`: the exit code.
fn run(monitor: u32, secs: u32) -> i32 {
    let gallery = match std::env::current_exe() {
        Ok(p) => p.with_file_name("gallery.exe"),
        Err(e) => {
            eprintln!("band_capture: {e}");
            return 1;
        }
    };
    let mut child = match Command::new(&gallery)
        .args(["--measure", "band", "--monitor", &monitor.to_string(), "--secs", &secs.to_string()])
        .stdout(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW.0)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("band_capture: {}: {e}", gallery.display());
            return 1;
        }
    };
    let started = Instant::now();
    let found = loop {
        if let Some(h) = window_of(child.id()) {
            break Some(h);
        }
        if started.elapsed() > APPEAR || child.try_wait().map(|s| s.is_some()).unwrap_or(true) {
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let Some(h) = found else {
        eprintln!("band_capture: the gallery's window did not appear");
        let _ = child.kill();
        let _ = child.wait();
        return 1;
    };
    std::thread::sleep(SETTLE);
    let result = client_on_screen(h).ok_or_else(|| "no client rect".to_string()).and_then(|client| {
        let cap = duplicate(h)?;
        let counts = capture(&cap, client, secs, &mut child)?;
        counts.measured(secs).map(|()| counts)
    });
    let counts = match result {
        Ok(c) => c,
        Err(e) => {
            println!("{{\"monitor\":{monitor},\"not_measured\":{:?}}}", e);
            let _ = child.kill();
            let _ = child.wait();
            return 3;
        }
    };
    let _ = child.wait();
    let mut out = String::new();
    if let Some(mut s) = child.stdout.take() {
        let _ = s.read_to_string(&mut out);
    }
    let gallery_line = out.lines().find(|l| l.starts_with('{'));
    println!("{}", line(monitor, &counts, gallery_line));
    0
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match parse(&args) {
        Ok((monitor, secs)) => run(monitor, secs),
        Err(e) => {
            eprintln!("band_capture: {e}");
            2
        }
    };
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(line: &str) -> Result<(u32, u32), String> {
        parse(&line.split_whitespace().map(str::to_string).collect::<Vec<_>>())
    }

    #[test]
    fn band_capture_parses_its_line() {
        assert_eq!(p("--monitor 0 --secs 60"), Ok((0, 60)));
        assert_eq!(p("--secs 5 --monitor 2"), Ok((2, 5)));
        for bad in
            ["", "--monitor 0", "--monitor x --secs 1", "--monitor 0 --secs 0", "--monitor 0 --monitor 1", "--fast"]
        {
            assert!(p(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn counts_and_line() {
        let mut c = Counts::default();
        for r in [Read::Clean, Read::Clean, Read::Torn { runs: 2 }, Read::Unreadable] {
            c.add(r);
        }
        assert_eq!((c.captured, c.clean, c.torn, c.unreadable), (4, 2, 1, 1));
        assert!((c.torn_pct() - 33.333).abs() < 0.01, "torn among the frames read");
        let l = line(1, &c, Some("{\"mode\":\"band\"}"));
        let v: serde_json::Value = serde_json::from_str(&l).unwrap();
        assert_eq!((v["monitor"].as_u64(), v["torn"].as_u64()), (Some(1), Some(1)));
        assert_eq!(v["gallery"]["mode"].as_str(), Some("band"));
        assert!(
            serde_json::from_str::<serde_json::Value>(&line(0, &Counts::default(), None)).unwrap()["gallery"].is_null()
        );
    }

    #[test]
    fn a_capture_that_read_too_few_rulers_is_not_measured() {
        let mut c = Counts::default();
        assert!(c.measured(60).is_err(), "no frame at all");
        for _ in 0..1000 {
            c.add(Read::Unreadable);
        }
        assert!(c.torn_pct().abs() < 1e-9 && c.measured(60).is_err(), "every frame unreadable: 0 torn is no pass");
        for _ in 0..599 {
            c.add(Read::Clean);
        }
        assert!(c.measured(60).is_err(), "599 rulers read in 60 s");
        c.add(Read::Torn { runs: 2 });
        assert_eq!(c.measured(60), Ok(()));
    }
}
