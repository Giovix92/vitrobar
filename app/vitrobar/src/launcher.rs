//! Fire-and-forget launching on a worker thread (its own COM STA): `ActivateApplication` is an
//! out-of-process call and `ShellExecuteEx` can take seconds, so neither runs on the UI thread.

use std::sync::mpsc::{channel, Sender};

use vitro_core::settings::Pin;
use vitro_win::launch::{
    launch_pin, open_stock_notifications, open_stock_quick_settings, reveal_in_folder, shell_open,
};
use vitro_win::power::{perform, PowerAction};
use windows::core::{Result, HSTRING};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};

#[derive(Debug, Clone)]
pub enum Job {
    Pin(Pin),
    QuickSettings,
    Notifications,
    /// A file, folder or URI (`ms-settings:`, `shell:AppsFolder\<id>`) opened by the shell.
    Open(String),
    /// "Open file location": Explorer on the item's folder, the item selected.
    Reveal(String),
    /// A power-menu action. A failure (Sleep without the privilege, say) is only logged.
    Power(PowerAction),
}

pub struct Launcher {
    tx: Sender<Job>,
}

/// Outcome of one job, for tests and diagnostics.
pub type Report = (Job, Result<()>);

struct ComApartment(bool);
impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances the successful CoInitializeEx on this thread.
            unsafe { CoUninitialize() };
        }
    }
}

fn run(job: &Job) -> Result<()> {
    match job {
        Job::Pin(p) => launch_pin(p),
        Job::QuickSettings => open_stock_quick_settings(),
        Job::Notifications => open_stock_notifications(),
        Job::Open(target) => shell_open(&HSTRING::from(target.as_str())),
        Job::Reveal(path) => reveal_in_folder(path),
        Job::Power(a) => perform(*a),
    }
}

impl Launcher {
    /// Failures are logged on stderr; `reports` additionally receives every outcome.
    pub fn start(reports: Option<Sender<Report>>) -> std::io::Result<Self> {
        let (tx, rx) = channel::<Job>();
        std::thread::Builder::new()
            .name("vitrobar-launch".into())
            .spawn(move || {
                // SAFETY: initialises COM for this worker thread only; balanced by the guard.
                let _com =
                    ComApartment(unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok());
                while let Ok(job) = rx.recv() {
                    let result = run(&job);
                    if let Err(e) = &result {
                        eprintln!("vitrobar: {job:?} failed: {e}");
                    }
                    if let Some(r) = &reports {
                        let _ = r.send((job, result));
                    }
                }
            })?;
        Ok(Self { tx })
    }

    /// Queues the job and returns at once.
    pub fn launch(&self, job: Job) {
        let _ = self.tx.send(job);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn launch_returns_immediately_and_fails_without_ui() {
        let (tx, rx) = channel();
        let launcher = Launcher::start(Some(tx)).unwrap();
        let pin = Pin {
            aumid: None,
            path: Some(r"C:\does\not\exist\vitrobar-test.exe".into()),
            name: "missing".into(),
            link: None,
        };
        let t = Instant::now();
        launcher.launch(Job::Pin(pin));
        assert!(t.elapsed() < Duration::from_millis(50), "{:?}", t.elapsed());
        // SEE_MASK_FLAG_NO_UI: the failure comes back as an error, no dialog waits for a click.
        let (job, result) = rx.recv_timeout(Duration::from_secs(20)).unwrap();
        assert!(matches!(job, Job::Pin(_)));
        assert!(result.is_err());
    }

    #[test]
    fn open_failures_and_dry_run_power_come_back_as_reports() {
        vitro_win::power::set_dry_run(true);
        let (tx, rx) = channel();
        let launcher = Launcher::start(Some(tx)).unwrap();
        launcher.launch(Job::Open(r"C:\does\not\exist\vitrobar-test.txt".into()));
        launcher.launch(Job::Power(PowerAction::ShutDown));
        let (job, result) = rx.recv_timeout(Duration::from_secs(20)).unwrap();
        assert!(matches!(job, Job::Open(_)));
        assert!(result.is_err());
        let (job, result) = rx.recv_timeout(Duration::from_secs(20)).unwrap();
        assert!(matches!(job, Job::Power(PowerAction::ShutDown)));
        assert!(result.is_ok());
        assert_eq!(
            vitro_win::power::last_dry_run(),
            Some(PowerAction::ShutDown)
        );
    }
}
