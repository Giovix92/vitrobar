//! The current media session for the music widget (spec §5.7): title, artist, playback state,
//! play / pause / skip, and the artwork for the detail panel.
//!
//! Event driven, nothing polls: [`Media::open`] starts one worker thread that owns the
//! `GlobalSystemMediaTransportControlsSessionManager`. `CurrentSessionChanged` and the current
//! session's `MediaPropertiesChanged` / `PlaybackInfoChanged` only queue a refresh for that
//! worker; it re-reads the session (a cross-process call into the player, which may be slow),
//! stores a [`MediaSnapshot`] and posts a payload-free wake-up (see `event_queue`) when it
//! changed. Commands and the artwork decode run on the same worker, so a hung player never
//! blocks the UI thread.
//!
//! The artwork is read only on request ([`Media::request_thumbnail`], while the detail panel is
//! open) and decoded by `Windows.Graphics.Imaging` (WIC) to a premultiplied BGRA square.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

use vitro_core::image::BgraImage;
use windows::core::Result;
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Imaging::{
    BitmapAlphaMode, BitmapBounds, BitmapDecoder, BitmapInterpolationMode, BitmapPixelFormat,
    BitmapTransform, ColorManagementMode, ExifOrientationMode,
};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};
use windows::Storage::Streams::IRandomAccessStreamReference;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::brightness::ComGuard;
use crate::event_queue::WakeQueue;

/// The artwork is never decoded larger than this (px).
pub const MAX_THUMBNAIL_PX: u32 = 512;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaSnapshot {
    /// The session's source app (an AUMID or an exe name).
    pub app: String,
    pub title: String,
    pub artist: String,
    pub playing: bool,
    pub can_prev: bool,
    pub can_next: bool,
    pub can_play_pause: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// The session, its track or its playback state changed: read [`Media::snapshot`].
    Session,
    /// The requested artwork is ready: [`Media::take_thumbnail`].
    Thumbnail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    PlayPause,
    Next,
    Prev,
}

enum Job {
    Refresh,
    Command(Command),
    Thumbnail(u32),
    Quit,
}

struct Shared {
    queue: WakeQueue<Change>,
    notify: isize,
    msg: u32,
    closed: AtomicBool,
    snapshot: Mutex<Option<MediaSnapshot>>,
    thumbnail: Mutex<Option<BgraImage>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    fn wake(&self, change: Change) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        if self.queue.push(change) && self.notify != 0 {
            // SAFETY: a payload-free post to a window of this process.
            if unsafe {
                PostMessageW(
                    Some(HWND(self.notify as *mut _)),
                    self.msg,
                    WPARAM(0),
                    LPARAM(0),
                )
            }
            .is_err()
            {
                self.queue.wake_failed();
            }
        }
    }
}

pub struct Media {
    shared: Arc<Shared>,
    jobs: Sender<Job>,
}

impl Media {
    /// Starts the worker. No media session (or no media manager at all) is not an error: the
    /// snapshot is simply `None` and the widget hides. `notify` receives a payload-free
    /// `wake_msg` on every change; on that message call [`Media::drain_changes`].
    pub fn open(notify: HWND, wake_msg: u32) -> Result<Self> {
        let shared = Arc::new(Shared {
            queue: WakeQueue::new(),
            notify: notify.0 as isize,
            msg: wake_msg,
            closed: AtomicBool::new(false),
            snapshot: Mutex::new(None),
            thumbnail: Mutex::new(None),
        });
        let (tx, rx) = channel();
        let sh = shared.clone();
        let jobs = tx.clone();
        std::thread::Builder::new()
            .name("vitro-media".into())
            .spawn(move || worker(sh, rx, jobs))
            .map_err(|_| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;
        let _ = tx.send(Job::Refresh);
        Ok(Self { shared, jobs: tx })
    }

    pub fn drain_changes(&self) -> Vec<Change> {
        self.shared.queue.drain()
    }

    /// The current session as last read; `None` with no session.
    pub fn snapshot(&self) -> Option<MediaSnapshot> {
        lock(&self.shared.snapshot).clone()
    }

    pub fn play_pause(&self) {
        let _ = self.jobs.send(Job::Command(Command::PlayPause));
    }

    pub fn next(&self) {
        let _ = self.jobs.send(Job::Command(Command::Next));
    }

    pub fn prev(&self) {
        let _ = self.jobs.send(Job::Command(Command::Prev));
    }

    /// Reads and decodes the current artwork to a `px` square (clamped to
    /// [`MAX_THUMBNAIL_PX`]); [`Change::Thumbnail`] follows, also when there is none.
    pub fn request_thumbnail(&self, px: u32) {
        let _ = self
            .jobs
            .send(Job::Thumbnail(px.clamp(1, MAX_THUMBNAIL_PX)));
    }

    /// The artwork decoded by the last [`Media::request_thumbnail`], once.
    pub fn take_thumbnail(&self) -> Option<BgraImage> {
        lock(&self.shared.thumbnail).take()
    }
}

impl Drop for Media {
    fn drop(&mut self) {
        // The worker unhooks every handler and exits; it is not joined (a player stuck in a
        // call must not hold up the UI thread).
        self.shared.closed.store(true, Ordering::Release);
        let _ = self.jobs.send(Job::Quit);
    }
}

/// The session the worker listens to, with its event tokens.
struct Bound {
    session: Session,
    props: i64,
    playback: i64,
}

impl Bound {
    fn unhook(self) {
        let _ = self.session.RemoveMediaPropertiesChanged(self.props);
        let _ = self.session.RemovePlaybackInfoChanged(self.playback);
    }
}

fn worker(shared: Arc<Shared>, rx: Receiver<Job>, jobs: Sender<Job>) {
    let _com = ComGuard::init();
    let manager = Manager::RequestAsync().and_then(|a| a.join()).ok();
    let mut token = None;
    if let Some(m) = &manager {
        let tx = jobs.clone();
        token = m
            .CurrentSessionChanged(&TypedEventHandler::new(move |_, _| {
                let _ = tx.send(Job::Refresh);
                Ok(())
            }))
            .ok();
    }
    let mut bound: Option<Bound> = None;
    while let Ok(job) = rx.recv() {
        let current = manager.as_ref().and_then(|m| m.GetCurrentSession().ok());
        match job {
            Job::Quit => break,
            Job::Refresh => {
                rebind(&mut bound, current.as_ref(), &jobs);
                let snap = current.as_ref().and_then(read_session);
                let mut cur = lock(&shared.snapshot);
                if *cur != snap {
                    *cur = snap;
                    drop(cur);
                    shared.wake(Change::Session);
                }
            }
            Job::Command(c) => {
                if let Some(s) = &current {
                    let r = match c {
                        Command::PlayPause => s.TryTogglePlayPauseAsync(),
                        Command::Next => s.TrySkipNextAsync(),
                        Command::Prev => s.TrySkipPreviousAsync(),
                    };
                    let _ = r.and_then(|a| a.join());
                }
            }
            Job::Thumbnail(px) => {
                let img = current.as_ref().and_then(|s| thumbnail(s, px).ok());
                *lock(&shared.thumbnail) = img;
                shared.wake(Change::Thumbnail);
            }
        }
    }
    if let Some(b) = bound.take() {
        b.unhook();
    }
    if let (Some(m), Some(t)) = (&manager, token) {
        let _ = m.RemoveCurrentSessionChanged(t);
    }
}

/// Listens to `current` (and no longer to the previously bound session).
fn rebind(bound: &mut Option<Bound>, current: Option<&Session>, jobs: &Sender<Job>) {
    if bound.as_ref().map(|b| &b.session) == current {
        return;
    }
    if let Some(b) = bound.take() {
        b.unhook();
    }
    let Some(s) = current else {
        return;
    };
    let tx = jobs.clone();
    let Ok(props) = s.MediaPropertiesChanged(&TypedEventHandler::new(move |_, _| {
        let _ = tx.send(Job::Refresh);
        Ok(())
    })) else {
        return;
    };
    let tx = jobs.clone();
    let Ok(playback) = s.PlaybackInfoChanged(&TypedEventHandler::new(move |_, _| {
        let _ = tx.send(Job::Refresh);
        Ok(())
    })) else {
        let _ = s.RemoveMediaPropertiesChanged(props);
        return;
    };
    *bound = Some(Bound {
        session: s.clone(),
        props,
        playback,
    });
}

fn read_session(s: &Session) -> Option<MediaSnapshot> {
    let props = s.TryGetMediaPropertiesAsync().ok()?.join().ok()?;
    let info = s.GetPlaybackInfo().ok()?;
    let controls = info.Controls().ok()?;
    let on = |r: Result<bool>| r.unwrap_or(false);
    Some(MediaSnapshot {
        app: s
            .SourceAppUserModelId()
            .map(|h| h.to_string())
            .unwrap_or_default(),
        title: props.Title().map(|h| h.to_string()).unwrap_or_default(),
        artist: props.Artist().map(|h| h.to_string()).unwrap_or_default(),
        playing: info.PlaybackStatus().ok() == Some(Status::Playing),
        can_prev: on(controls.IsPreviousEnabled()),
        can_next: on(controls.IsNextEnabled()),
        can_play_pause: on(controls.IsPlayPauseToggleEnabled()),
    })
}

/// How to fill a `px` square from a `w`×`h` image: the scaled size (the shorter side becomes
/// `px`) and the top-left corner of the centred crop.
pub fn cover_crop(w: u32, h: u32, px: u32) -> (u32, u32, u32, u32) {
    let short = w.min(h).max(1) as f64;
    let scale = f64::from(px) / short;
    let sw = ((f64::from(w) * scale).round() as u32).max(px);
    let sh = ((f64::from(h) * scale).round() as u32).max(px);
    (sw, sh, (sw - px) / 2, (sh - px) / 2)
}

fn thumbnail(s: &Session, px: u32) -> Result<BgraImage> {
    let props = s.TryGetMediaPropertiesAsync()?.join()?;
    let reference: IRandomAccessStreamReference = props.Thumbnail()?;
    let stream = reference.OpenReadAsync()?.join()?;
    let decoder = BitmapDecoder::CreateAsync(&stream)?.join()?;
    let (w, h) = (
        decoder.OrientedPixelWidth()?,
        decoder.OrientedPixelHeight()?,
    );
    if w == 0 || h == 0 {
        return Err(windows::Win32::Foundation::E_FAIL.into());
    }
    let (sw, sh, x, y) = cover_crop(w, h, px);
    let t = BitmapTransform::new()?;
    t.SetInterpolationMode(BitmapInterpolationMode::Fant)?;
    t.SetScaledWidth(sw)?;
    t.SetScaledHeight(sh)?;
    t.SetBounds(BitmapBounds {
        X: x,
        Y: y,
        Width: px,
        Height: px,
    })?;
    let data = decoder
        .GetPixelDataTransformedAsync(
            BitmapPixelFormat::Bgra8,
            BitmapAlphaMode::Premultiplied,
            &t,
            ExifOrientationMode::RespectExifOrientation,
            ColorManagementMode::DoNotColorManage,
        )?
        .join()?;
    let pixels = data.DetachPixelData()?.to_vec();
    let img = BgraImage {
        width: px,
        height: px,
        pixels,
    };
    if img.is_valid() {
        Ok(img)
    } else {
        Err(windows::Win32::Foundation::E_FAIL.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cover_crop_fills_the_square_from_the_middle() {
        assert_eq!(cover_crop(300, 300, 64), (64, 64, 0, 0));
        // Wide: height becomes 64, the sides are cut.
        assert_eq!(cover_crop(640, 320, 64), (128, 64, 32, 0));
        // Tall.
        assert_eq!(cover_crop(100, 400, 50), (50, 200, 0, 75));
        // Tiny source is scaled up.
        assert_eq!(cover_crop(16, 16, 64), (64, 64, 0, 0));
    }

    /// Read-only: opens the media manager and reads the current session (never sends a
    /// command). Run by hand with something playing.
    #[test]
    #[ignore]
    fn reads_the_current_session() {
        let m = Media::open(HWND::default(), 0).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(2));
        eprintln!("snapshot: {:?}", m.snapshot());
        m.request_thumbnail(64);
        std::thread::sleep(std::time::Duration::from_secs(2));
        if let Some(t) = m.take_thumbnail() {
            assert_eq!((t.width, t.height), (64, 64));
            assert!(t.is_valid());
        }
    }
}
