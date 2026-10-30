//! Asynchronous icon loading: one worker thread (its own COM STA) runs the slow loads
//! (`SendMessageTimeout` to busy windows, shell `GetImage` on network paths) and posts each
//! result back to the UI thread as `WM_APP_ICON`. The UI draws a placeholder meanwhile.

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use vitro_core::image::BgraImage;
use vitro_win::icons::{load_icon, IconSource};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::notify::WM_APP_ICON;

/// A failed source is retried at most once per this interval.
const RETRY_AFTER: Duration = Duration::from_secs(30);
/// Bar icons never need more than this (carry item 13).
pub const MAX_ICON_PX: u32 = 96;
/// Above this many entries, entries no longer referenced are dropped.
const SOFT_CAP: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum IconSrc {
    Aumid(String),
    Path(String),
    Window(u64),
}

impl IconSrc {
    fn as_source(&self) -> IconSource<'_> {
        match self {
            IconSrc::Aumid(a) => IconSource::Aumid(a),
            IconSrc::Path(p) => IconSource::Path(p),
            IconSrc::Window(h) => IconSource::Window(HWND(*h as usize as *mut _)),
        }
    }
}

type Key = (IconSrc, u32);

enum Slot {
    Pending,
    Ready(Arc<BgraImage>),
    Failed(Instant),
}

pub enum Lookup<'a> {
    Ready(&'a Arc<BgraImage>),
    Pending,
    Failed,
}

pub struct IconLoader {
    tx: Option<Sender<Key>>,
    rx: Receiver<(Key, Option<BgraImage>)>,
    slots: HashMap<Key, Slot>,
}

struct ComApartment(bool);
impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances the successful CoInitializeEx on this thread.
            unsafe { CoUninitialize() };
        }
    }
}

impl IconLoader {
    /// Starts the worker; results are announced to `notify` with `WM_APP_ICON`.
    pub fn start(notify: HWND) -> std::io::Result<Self> {
        let (tx, worker_rx) = channel::<Key>();
        let (worker_tx, rx) = channel::<(Key, Option<BgraImage>)>();
        let notify = notify.0 as isize;
        std::thread::Builder::new()
            .name("vitrobar-icons".into())
            .spawn(move || {
                // SAFETY: initialises COM for this worker thread only; balanced by the guard.
                let _com =
                    ComApartment(unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok());
                while let Ok(key) = worker_rx.recv() {
                    let image = load_icon(key.0.as_source(), key.1);
                    if worker_tx.send((key, image)).is_err() {
                        break;
                    }
                    // SAFETY: posting carries no pointer; a destroyed window just fails.
                    unsafe {
                        let _ = PostMessageW(
                            Some(HWND(notify as *mut _)),
                            WM_APP_ICON,
                            WPARAM(0),
                            LPARAM(0),
                        );
                    }
                }
            })?;
        Ok(Self {
            tx: Some(tx),
            rx,
            slots: HashMap::new(),
        })
    }

    /// The icon if loaded; otherwise queues it (once, or again after a failure older than
    /// 30 s) and reports pending/failed.
    pub fn get(&mut self, src: &IconSrc, size_px: u32) -> Lookup<'_> {
        let key = (src.clone(), size_px.clamp(1, MAX_ICON_PX));
        let request = match self.slots.get(&key) {
            None => true,
            Some(Slot::Failed(at)) => at.elapsed() >= RETRY_AFTER,
            _ => false,
        };
        if request {
            if let Some(tx) = &self.tx {
                if tx.send(key.clone()).is_ok() {
                    self.slots.insert(key.clone(), Slot::Pending);
                }
            }
        }
        match self.slots.get(&key) {
            Some(Slot::Ready(img)) => Lookup::Ready(img),
            Some(Slot::Failed(_)) => Lookup::Failed,
            _ => Lookup::Pending,
        }
    }

    /// Takes the finished loads; true if any arrived.
    pub fn drain(&mut self) -> bool {
        let mut any = false;
        while let Ok((key, image)) = self.rx.try_recv() {
            let slot = match image {
                Some(img) => Slot::Ready(Arc::new(img)),
                None => Slot::Failed(Instant::now()),
            };
            self.slots.insert(key, slot);
            any = true;
        }
        any
    }

    /// Keeps the map bounded: past the cap, forget entries `keep` does not want.
    pub fn trim(&mut self, keep: impl Fn(&IconSrc) -> bool) {
        if self.slots.len() > SOFT_CAP {
            self.slots
                .retain(|(src, _), slot| matches!(slot, Slot::Pending) || keep(src));
        }
    }
}

impl Drop for IconLoader {
    fn drop(&mut self) {
        // Closing the channel ends the worker after its current load. It is not joined: a
        // load can take long (network shell item) and the process is exiting anyway.
        self.tx.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_until(loader: &mut IconLoader, src: &IconSrc, size: u32) -> bool {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(10) {
            loader.drain();
            if !matches!(loader.get(src, size), Lookup::Pending) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn loads_off_thread_and_backs_off_failures() {
        // No notify window: the wake-up post simply fails; results are still delivered.
        let mut loader = IconLoader::start(HWND::default()).unwrap();
        let good = IconSrc::Path(r"C:\Windows\explorer.exe".into());
        let bad = IconSrc::Path(r"C:\does\not\exist\nothing.exe".into());
        assert!(matches!(loader.get(&good, 32), Lookup::Pending));
        assert!(matches!(loader.get(&bad, 32), Lookup::Pending));
        assert!(wait_until(&mut loader, &good, 32));
        assert!(wait_until(&mut loader, &bad, 32));
        match loader.get(&good, 32) {
            Lookup::Ready(img) => assert_eq!((img.width, img.height), (32, 32)),
            _ => panic!("explorer.exe icon did not load"),
        }
        // A failure is not re-requested within 30 s.
        assert!(matches!(loader.get(&bad, 32), Lookup::Failed));
        assert!(matches!(loader.get(&bad, 32), Lookup::Failed));
        // Requests are capped at 96 px.
        assert!(matches!(loader.get(&good, 400), Lookup::Pending));
        assert!(wait_until(&mut loader, &good, 400));
        match loader.get(&good, 400) {
            Lookup::Ready(img) => assert_eq!(img.width, MAX_ICON_PX),
            _ => panic!("capped load failed"),
        }
    }
}
