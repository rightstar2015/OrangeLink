//! Playback through Windows Media Foundation: the `IMFMediaEngine` decodes on
//! the GPU (DXVA through a D3D11 device) and plays the sound itself; each new
//! frame is drawn at preview size into a D3D11 texture, read back and handed to
//! egui. One thread per player owns the engine and every COM object, so the UI
//! thread only sends commands and reads a shared state.
//!
//! Adapted from Prime Files (`crates/nohrs-pages/src/explorer/media.rs`, the
//! media engine's frame-server loop), MIT License, Copyright (c) 2025 Nohrs
//! contributors, (c) 2026 Prime Files contributors. Prime Files copies frames
//! into a WIC bitmap without a D3D device; here a DXGI device manager is added so
//! decoding runs on the GPU.

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

/// A decoded frame, RGBA.
pub struct Frame {
    pub size: [usize; 2],
    pub rgba: Vec<u8>,
    /// The last seek done before it was drawn, to tell frames from before a seek.
    seek: u64,
}

enum Cmd {
    Play,
    Pause,
    /// Where, and whether the nearest keyframe will do (quick, for dragging).
    Seek(f64, bool),
    Volume(f32),
    /// Playback speed, 1 = normal.
    Rate(f64),
}

/// What the engine thread last saw, refreshed every few milliseconds.
#[derive(Default)]
struct State {
    /// The last command applied.
    applied: u64,
    position: f64,
    /// When `position` was read, to run it on while playing.
    at: Option<Instant>,
    playing: bool,
    ended: bool,
    duration: f64,
    error: Option<String>,
}

/// The UI side of one Media Foundation player.
pub struct MfPlayer {
    /// `None` once dropping has begun: closing it is what ends the engine thread.
    commands: Option<Sender<(u64, Cmd)>>,
    /// The engine thread, joined when the player is dropped.
    engine: Option<thread::JoinHandle<()>>,
    state: Arc<Mutex<State>>,
    frames: Receiver<Frame>,
    /// Commands sent so far; the state is current once it has applied them all.
    sent: u64,
    /// The last seek command; frames drawn before it are stale.
    last_seek: u64,
    /// Where the last rough seek went: a drag repeats it every frame while the
    /// pointer rests, and each repeat would start the seek over.
    last_rough: Option<f64>,
    /// Where playback should be by the last command, until the engine catches up.
    expect: f64,
    expect_since: Option<Instant>,
    playing: bool,
    duration: f64,
    /// Playback speed set by `set_rate`.
    rate: f64,
}

impl MfPlayer {
    /// Opens `path` on a new thread. `video` says the file should have a
    /// picture: a video the engine opens without one (a codec Windows lacks)
    /// counts as a failure, so it can be played some other way.
    pub fn open(path: &std::path::Path, duration: f64, video: bool, max_width: u32, volume: f32) -> Self {
        Self::open_tracked(path, duration, video, max_width, volume, Arc::new(AtomicBool::new(false)))
    }

    /// `open`, with `finished` set once the engine thread has shut everything down.
    fn open_tracked(path: &std::path::Path, duration: f64, video: bool, max_width: u32, volume: f32, finished: Arc<AtomicBool>) -> Self {
        let (commands, rx) = mpsc::channel();
        let (frame_tx, frames) = mpsc::sync_channel(2);
        let state = Arc::new(Mutex::new(State { duration, ..Default::default() }));
        let shared = state.clone();
        let path = path.to_path_buf();
        let engine = thread::spawn(move || {
            run(&path, video, max_width, volume, rx, &shared, frame_tx);
            finished.store(true, Ordering::SeqCst);
        });
        Self { commands: Some(commands), engine: Some(engine), state, frames, sent: 0, last_seek: 0, last_rough: None, expect: 0.0, expect_since: None, playing: false, duration, rate: 1.0 }
    }

    fn send(&mut self, cmd: Cmd) {
        self.sent += 1;
        if let Some(commands) = &self.commands {
            let _ = commands.send((self.sent, cmd));
        }
    }

    pub fn play(&mut self) {
        if self.playing {
            return;
        }
        let mut from = self.position();
        if self.duration > 0.0 && from >= self.duration - 0.05 {
            from = 0.0; // the engine starts over from the end, too
        }
        self.expect = from;
        self.expect_since = Some(Instant::now());
        self.playing = true;
        self.send(Cmd::Play);
    }

    pub fn pause(&mut self) {
        if !self.playing {
            return;
        }
        self.expect = self.position();
        self.expect_since = None;
        self.playing = false;
        self.send(Cmd::Pause);
    }

    /// `rough`: land on the nearest keyframe, about 10× quicker than an exact
    /// seek (which decodes on from the keyframe before), for dragging.
    pub fn seek(&mut self, pos: f64, rough: bool) {
        if rough && self.last_rough.is_some_and(|p| (p - pos).abs() < 0.01) {
            return;
        }
        // Within a drag, a frame from any of its seeks is wanted: one from the
        // newest would never come while the pointer keeps moving.
        let dragging = rough && self.last_rough.is_some();
        self.last_rough = rough.then_some(pos);
        self.expect = pos;
        self.expect_since = self.playing.then(Instant::now);
        self.send(Cmd::Seek(pos, rough));
        if !dragging {
            self.last_seek = self.sent;
        }
    }

    pub fn set_volume(&mut self, v: f32) {
        self.send(Cmd::Volume(v));
    }

    /// Plays at `rate` times normal speed (positive only: the engine can't
    /// play most files backwards).
    pub fn set_rate(&mut self, rate: f64) {
        if (rate - self.rate).abs() < 1e-9 {
            return;
        }
        // Run the expected position on from here at the new speed.
        self.expect = self.position();
        self.expect_since = self.playing.then(Instant::now);
        self.rate = rate;
        self.send(Cmd::Rate(rate));
    }

    /// Lands on the nearest keyframe at `pos` without a full seek (stepping backwards).
    pub fn seek_rough(&mut self, pos: f64) {
        self.seek(pos, true);
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn position(&self) -> f64 {
        let s = crate::lock(&self.state);
        let pos = if s.applied >= self.sent && s.at.is_some() {
            s.position + if s.playing { s.at.map(|t| t.elapsed().as_secs_f64() * self.rate).unwrap_or(0.0) } else { 0.0 }
        } else {
            self.expect + self.expect_since.map(|t| t.elapsed().as_secs_f64() * self.rate).unwrap_or(0.0)
        };
        if self.duration > 0.0 {
            pos.clamp(0.0, self.duration)
        } else {
            pos.max(0.0)
        }
    }

    /// The newest frame, a failure, the length once known; and notices the end.
    pub fn poll(&mut self) -> (Option<Frame>, Option<String>, Option<f64>) {
        let frame = self.frames.try_iter().filter(|f| f.seek >= self.last_seek).last();
        let (error, duration, ended) = {
            let mut s = crate::lock(&self.state);
            (s.error.take(), (s.duration > 0.0).then_some(s.duration), s.ended && s.applied >= self.sent)
        };
        if let Some(d) = duration {
            self.duration = d;
        }
        if ended && self.playing {
            self.playing = false;
            self.expect = self.duration;
            self.expect_since = None;
        }
        (frame, error, duration)
    }
}

impl Drop for MfPlayer {
    /// Closes the command channel, which ends the engine thread's loop, and waits
    /// for the thread to end after shutting the engine, Direct3D and Media Foundation down. A
    /// thread left to finish by itself can still be inside that shutdown when the
    /// process exits, and the process then dies with STATUS_INVALID_PARAMETER
    /// (seen at the end of `cargo test` once a PDF had also been rendered).
    /// The wait is bounded so a stuck engine can't hold the window open.
    fn drop(&mut self) {
        self.commands = None;
        if let Some(engine) = self.engine.take() {
            crate::join_within(engine, Duration::from_secs(3));
        }
    }
}

// ---- The engine thread.

use windows::{
    core::{implement, Interface, BSTR},
    Win32::{
        Foundation::{HMODULE, RECT},
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_HARDWARE,
            Direct3D11::*,
            Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
        },
        Media::MediaFoundation::*,
        System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER},
    },
};

// The engine insists on a listener; the thread polls the engine instead.
#[implement(IMFMediaEngineNotify)]
struct Listener;

impl IMFMediaEngineNotify_Impl for Listener_Impl {
    fn EventNotify(&self, _event: u32, _param1: usize, _param2: u32) -> windows::core::Result<()> {
        Ok(())
    }
}

/// `MF_MEDIA_ENGINE_READY.HAVE_METADATA`: the length and size are known.
const HAVE_METADATA: u16 = 1;

fn run(
    path: &std::path::Path,
    video: bool,
    max_width: u32,
    volume: f32,
    commands: Receiver<(u64, Cmd)>,
    state: &Mutex<State>,
    frames: SyncSender<Frame>,
) {
    let fail = |msg: String| crate::lock(state).error = Some(msg);
    // Left last, after MFShutdown: a thread still in the apartment at exit had COM
    // tearing down under it (issue #4).
    let _com = crate::com::Apartment::mta();
    // SAFETY: no pointers; this thread is the engine's own, set up once here and
    // shut down at the end of this function.
    unsafe {
        if let Err(e) = MFStartup(MF_VERSION, MFSTARTUP_FULL) {
            fail(format!("Media Foundation 無法啟動：{}", e.message()));
            return;
        }
    }
    match Engine::open(path, volume) {
        Ok(mut engine) => {
            if let Err(e) = engine.run(video, max_width, &commands, state, &frames) {
                fail(e);
            }
            // SAFETY: the engine was made on this thread and is not used after this.
            unsafe {
                let _ = engine.engine.Shutdown();
            }
        }
        Err(e) => fail(format!("Media Foundation 無法開啟：{}", e.message())),
    }
    // SAFETY: pairs with the MFStartup above, after the engine is shut down and dropped.
    unsafe {
        let _ = MFShutdown();
    }
}

struct Engine {
    engine: IMFMediaEngine,
    _manager: IMFDXGIDeviceManager,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    /// The frame drawn by the engine, and its CPU-readable copy, at one size.
    target: Option<(ID3D11Texture2D, ID3D11Texture2D, u32, u32)>,
}

impl Engine {
    fn open(path: &std::path::Path, volume: f32) -> windows::core::Result<Self> {
        // SAFETY: COM and Media Foundation are started on this thread (`run`). Every
        // out-parameter is a local Option the call fills, checked before use; the
        // strings passed are owned values that outlive the calls.
        unsafe {
            let mut device = None;
            let mut context = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
            let device = device.ok_or_else(windows::core::Error::empty)?;
            let context = context.ok_or_else(windows::core::Error::empty)?;
            // The engine decodes on its own threads with the same device.
            let _ = device.cast::<ID3D11Multithread>()?.SetMultithreadProtected(true);

            let mut token = 0u32;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
            let manager = manager.ok_or_else(windows::core::Error::empty)?;
            manager.ResetDevice(&device, token)?;

            let factory: IMFMediaEngineClassFactory = CoCreateInstance(&CLSID_MFMediaEngineClassFactory, None, CLSCTX_INPROC_SERVER)?;
            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 3)?;
            let attributes = attributes.ok_or_else(windows::core::Error::empty)?;
            let listener: IMFMediaEngineNotify = Listener.into();
            attributes.SetUnknown(&MF_MEDIA_ENGINE_CALLBACK, &listener)?;
            attributes.SetUnknown(&MF_MEDIA_ENGINE_DXGI_MANAGER, &manager)?;
            attributes.SetUINT32(&MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM.0 as u32)?;
            let engine = factory.CreateInstance(0, &attributes)?;
            engine.SetVolume(volume as f64)?;
            engine.SetSource(&BSTR::from(path.to_string_lossy().as_ref()))?;
            Ok(Self { engine, _manager: manager, device, context, target: None })
        }
    }

    fn run(
        &mut self,
        video: bool,
        max_width: u32,
        commands: &Receiver<(u64, Cmd)>,
        state: &Mutex<State>,
        frames: &SyncSender<Frame>,
    ) -> Result<(), String> {
        // Another reference to the engine, so `grab` can borrow `self`.
        let e = &self.engine.clone();
        let mut applied = 0;
        // Seeks wait for the length to be known; the engine refuses them before.
        let mut seek_to: Option<(u64, f64, bool)> = None;
        // The last seek handed to the engine.
        let mut sought = 0;
        // When it was handed over, and the last seek whose frame has gone out.
        let mut sought_at = Instant::now();
        let mut shown = 0;
        let mut last_frame = i64::MIN;
        let opened = Instant::now();
        loop {
            match commands.recv_timeout(Duration::from_millis(8)) {
                Ok((n, cmd)) => {
                    applied = n;
                    // SAFETY: method calls on the engine this thread made and owns.
                    unsafe {
                        match cmd {
                            Cmd::Play => {
                                if e.IsEnded().as_bool() {
                                    let _ = e.SetCurrentTime(0.0);
                                }
                                let _ = e.Play();
                            }
                            Cmd::Pause => {
                                let _ = e.Pause();
                            }
                            Cmd::Seek(t, rough) => seek_to = Some((n, t, rough)),
                            Cmd::Volume(v) => {
                                let _ = e.SetVolume(v as f64);
                            }
                            Cmd::Rate(r) => {
                                let _ = e.SetPlaybackRate(r);
                            }
                        }
                    }
                    // Take every waiting command before reading the state.
                    continue;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }
            // SAFETY: method calls on the engine this thread made and owns; `grab`
            // reads only what it maps (see there).
            unsafe {
                if let Ok(err) = e.GetError() {
                    let code = err.GetErrorCode();
                    if code != 0 {
                        // The extended HRESULT's text is usually a bare "catastrophic
                        // failure"; the code says more. It goes to the log.
                        let detail = err.GetExtendedErrorCode().err().map(|x| format!("{:#010x}", x.code().0)).unwrap_or_default();
                        let what = match code {
                            3 => "Windows 解碼這個檔案時出錯",
                            4 => "Windows 不支援這個檔案的格式",
                            _ => "Windows 無法播放這個檔案",
                        };
                        return Err(format!("{what}（Media Foundation {code} {detail}）"));
                    }
                }
                let ready = e.GetReadyState() >= HAVE_METADATA;
                if !ready {
                    if opened.elapsed() > Duration::from_secs(15) {
                        return Err("Media Foundation 開啟檔案逾時".into());
                    }
                    continue;
                }
                let has_video = e.HasVideo().as_bool();
                if video && !has_video {
                    return Err("Windows 沒有這個影片格式的解碼器".into());
                }
                // A rough seek waits until the one before has landed and its frame
                // gone out (only the newest is kept), so a drag shows frames
                // instead of starting over forever.
                let busy = e.IsSeeking().as_bool() || (shown < sought && sought_at.elapsed() < Duration::from_millis(120));
                if let Some((n, t, rough)) = seek_to.filter(|s| !(s.2 && busy)) {
                    seek_to = None;
                    match e.cast::<IMFMediaEngineEx>().ok().filter(|_| rough) {
                        Some(ex) => {
                            let _ = ex.SetCurrentTimeEx(t, MF_MEDIA_ENGINE_SEEK_MODE_APPROXIMATE);
                        }
                        None => {
                            let _ = e.SetCurrentTime(t);
                        }
                    }
                    sought = n;
                    sought_at = Instant::now();
                }
                // The engine presents frames when asked for the tick, so ask every
                // time; but mid-seek it may still hold the frame from before.
                if has_video {
                    if let Ok(t) = e.OnVideoStreamTick() {
                        if t != last_frame && !e.IsSeeking().as_bool() {
                            if let Some(frame) = self.grab(max_width, sought) {
                                match frames.try_send(frame) {
                                    // A full queue: the UI is behind; try again next tick.
                                    Err(mpsc::TrySendError::Full(_)) => {}
                                    Err(mpsc::TrySendError::Disconnected(_)) => return Ok(()),
                                    Ok(()) => {
                                        last_frame = t;
                                        shown = sought;
                                    }
                                }
                            }
                        }
                    }
                }
                let duration = e.GetDuration();
                let mut s = crate::lock(state);
                // Mid-seek the engine still reports the time from before. Until it
                // lands, the UI goes on from where it asked to be, so quick ←/→
                // presses add up instead of each starting from the old time.
                if seek_to.is_none() && !e.IsSeeking().as_bool() {
                    s.applied = applied;
                    s.position = e.GetCurrentTime();
                    s.at = Some(Instant::now());
                }
                s.ended = e.IsEnded().as_bool();
                s.playing = !e.IsPaused().as_bool() && !s.ended && !e.IsSeeking().as_bool();
                if duration.is_finite() && duration > 0.0 {
                    s.duration = duration;
                }
            }
        }
    }

    /// Draws the current frame no wider than `max_width` and reads it back.
    fn grab(&mut self, max_width: u32, seek: u64) -> Option<Frame> {
        let (mut w, mut h) = (0u32, 0u32);
        // SAFETY: writes two u32s we own.
        unsafe { self.engine.GetNativeVideoSize(Some(&mut w), Some(&mut h)) }.ok()?;
        let [w, h] = crate::player::preview_size(w, h, max_width)?.map(|v| v as u32);
        if !matches!(self.target, Some((_, _, tw, th)) if tw == w && th == h) {
            self.target = Some(self.textures(w, h)?);
        }
        let (tex, staging, _, _) = self.target.as_ref()?;
        let rect = RECT { left: 0, top: 0, right: w as i32, bottom: h as i32 };
        let black = MFARGB { rgbBlue: 0, rgbGreen: 0, rgbRed: 0, rgbAlpha: 255 };
        // SAFETY: both textures are `w`×`h` BGRA (made for this size in `textures`).
        // The staging copy is read only between a successful Map and Unmap, row by
        // row at `RowPitch`, `w * 4` bytes each, which fits inside every row.
        unsafe {
            self.engine.TransferVideoFrame(tex, None, &rect, Some(&black)).ok()?;
            self.context.CopyResource(staging, tex);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).ok()?;
            let row = w as usize * 4;
            let mut rgba = Vec::with_capacity(row * h as usize);
            for y in 0..h as usize {
                // The mapped texture, valid until `Unmap`; rows are `RowPitch` apart.
                let line = std::slice::from_raw_parts((mapped.pData as *const u8).add(y * mapped.RowPitch as usize), row);
                for px in line.as_chunks::<4>().0 {
                    rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
                }
            }
            self.context.Unmap(staging, 0);
            Some(Frame { size: [w as usize, h as usize], rgba, seek })
        }
    }

    fn textures(&self, w: u32, h: u32) -> Option<(ID3D11Texture2D, ID3D11Texture2D, u32, u32)> {
        let mut desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut tex = None;
        let mut staging = None;
        // SAFETY: the description outlives both calls, and each fills a local Option.
        unsafe {
            self.device.CreateTexture2D(&desc, None, Some(&mut tex)).ok()?;
            desc.Usage = D3D11_USAGE_STAGING;
            desc.BindFlags = 0;
            desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            self.device.CreateTexture2D(&desc, None, Some(&mut staging)).ok()?;
        }
        Some((tex?, staging?, w, h))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dropping a player must not return while its engine thread is still shutting
    /// down: such a thread was left running when the test process exited.
    #[test]
    fn dropping_a_player_waits_for_its_engine_thread() {
        // The first player of a process can take seconds to start Media Foundation,
        // longer than dropping waits; let that one end by itself, so the drops below
        // are the usual short ones.
        let warm = Arc::new(AtomicBool::new(false));
        drop(MfPlayer::open_tracked(std::path::Path::new(r"C:
o\sucholder\clip.mp4"), 0.0, false, 320, 1.0, warm.clone()));
        let start = Instant::now();
        while !warm.load(Ordering::SeqCst) && start.elapsed() < Duration::from_secs(60) {
            thread::sleep(Duration::from_millis(50));
        }
        for _ in 0..5 {
            let finished = Arc::new(AtomicBool::new(false));
            let player = MfPlayer::open_tracked(std::path::Path::new(r"C:
o\sucholder\clip.mp4"), 0.0, false, 320, 1.0, finished.clone());
            drop(player);
            assert!(finished.load(Ordering::SeqCst), "the engine thread was still running after the player was dropped");
        }
    }
}
