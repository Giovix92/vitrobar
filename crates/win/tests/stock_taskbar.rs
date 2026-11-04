//! Taking over and restoring the stock taskbar. These change the real shell state (auto-hide,
//! the taskbar's visibility, a RunOnce value), so they are ignored by default. Run them on a
//! dev machine, one at a time:
//!
//! `cargo test -p vitro-win --test stock_taskbar -- --ignored --test-threads=1`
//!
//! Every test ends with the shell exactly as it found it.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use vitro_core::settings::StockTaskbar;
use vitro_win::stock_taskbar::{
    autohide_now, delete_runonce, restore_stock_taskbar, runonce_exists, show_stock_taskbar,
    stock_taskbar_visible, take_over_stock_taskbar, RestoreRecord, ShellLock,
};

fn temp_record(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vitrobar-test-{}", std::process::id()));
    let p = dir.join(name).join("restore.json");
    let _ = std::fs::remove_file(&p);
    p
}

fn temp_missing() -> PathBuf {
    temp_record("missing")
}

/// Waits (briefly) for the asynchronous show/hide of Explorer's windows to land.
fn eventually(mut f: impl FnMut() -> bool) -> bool {
    // Generous: right after a take-over or a restore Explorer can take several seconds to
    // move its taskbar, more so in a full run of the ignored tests one after another.
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        if f() {
            return true;
        }
        if Instant::now() > end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Puts the shell back however the test ends (also on a panic half-way). The RunOnce value
/// these tests make points at the test binary, which cannot restore anything at sign-in, so
/// it is deleted even when the restore keeps it (record unreadable, Explorer gone).
struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = restore_stock_taskbar(&self.0);
        let _ = delete_runonce();
        if let Some(dir) = self.0.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

#[test]
#[ignore = "changes the real stock taskbar"]
fn record_written_before_change_and_removed_after_restore() {
    let original = autohide_now();
    assert!(!runonce_exists(), "a VitrobarRestore value is already set");
    let p = temp_record("record");
    let _c = Cleanup(p.clone());
    take_over_stock_taskbar(StockTaskbar::AutoHide, &p).unwrap();
    assert!(p.exists());
    let text = std::fs::read_to_string(&p).unwrap();
    assert_eq!(
        RestoreRecord::from_json(&text),
        Some(RestoreRecord {
            original_autohide: original
        })
    );
    assert!(runonce_exists());
    assert!(autohide_now());
    restore_stock_taskbar(&p).unwrap();
    assert!(!p.exists());
    assert_eq!(autohide_now(), original);
    assert!(!runonce_exists());
    assert!(eventually(stock_taskbar_visible));
}

#[test]
#[ignore = "changes the real stock taskbar"]
fn restore_without_record_is_noop() {
    let original = autohide_now();
    assert!(restore_stock_taskbar(&temp_missing()).is_ok());
    assert_eq!(autohide_now(), original);
    assert!(!runonce_exists());
}

#[test]
#[ignore = "changes the real stock taskbar"]
fn restore_twice_is_safe() {
    let original = autohide_now();
    let p = temp_record("twice");
    let _c = Cleanup(p.clone());
    take_over_stock_taskbar(StockTaskbar::Hidden, &p).unwrap();
    assert!(
        eventually(|| !stock_taskbar_visible()),
        "Hidden mode leaves the stock taskbar visible"
    );
    restore_stock_taskbar(&p).unwrap();
    restore_stock_taskbar(&p).unwrap();
    assert!(!p.exists());
    assert_eq!(autohide_now(), original);
    assert!(!runonce_exists());
    assert!(eventually(stock_taskbar_visible));
}

#[test]
#[ignore = "changes the real stock taskbar"]
fn second_take_over_keeps_the_first_original() {
    // A run that crashed left its record behind: the next run must not record the
    // auto-hide it set itself as "the original".
    let original = autohide_now();
    let p = temp_record("crash");
    let _c = Cleanup(p.clone());
    take_over_stock_taskbar(StockTaskbar::AutoHide, &p).unwrap();
    take_over_stock_taskbar(StockTaskbar::AutoHide, &p).unwrap();
    let text = std::fs::read_to_string(&p).unwrap();
    assert_eq!(
        RestoreRecord::from_json(&text).map(|r| r.original_autohide),
        Some(original)
    );
    restore_stock_taskbar(&p).unwrap();
    assert_eq!(autohide_now(), original);
    assert!(!runonce_exists());
}

#[test]
#[ignore = "changes the real stock taskbar"]
fn unreadable_record_is_replaced_by_the_windows_default() {
    // A crashed run left a torn record behind while auto-hide is (Vitrobar's) on: that "on"
    // must not become the original.
    let original = autohide_now();
    let p = temp_record("torn");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, "{\"original_autohide\":tr").unwrap();
    let _c = Cleanup(p.clone());
    take_over_stock_taskbar(StockTaskbar::AutoHide, &p).unwrap();
    assert!(autohide_now());
    let text = std::fs::read_to_string(&p).unwrap();
    assert_eq!(
        RestoreRecord::from_json(&text),
        Some(RestoreRecord {
            original_autohide: false
        })
    );
    // Put the machine's real original back, whatever it is.
    RestoreRecord {
        original_autohide: original,
    }
    .write(&p)
    .unwrap();
    restore_stock_taskbar(&p).unwrap();
    assert_eq!(autohide_now(), original);
    assert!(!runonce_exists());
}

#[test]
#[ignore = "changes the real stock taskbar"]
fn show_works_without_a_record() {
    // Another instance (or RunOnce --restore) consumed the record while the taskbar is
    // hidden: a restore finds nothing to do, the explicit show still brings it back.
    let original = autohide_now();
    let p = temp_record("consumed");
    let _c = Cleanup(p.clone());
    take_over_stock_taskbar(StockTaskbar::Hidden, &p).unwrap();
    assert!(eventually(|| !stock_taskbar_visible()));
    let other = temp_missing();
    restore_stock_taskbar(&other).unwrap();
    show_stock_taskbar();
    assert!(eventually(stock_taskbar_visible));
    restore_stock_taskbar(&p).unwrap();
    assert_eq!(autohide_now(), original);
    assert!(!runonce_exists());
}

/// The sign-in race: RunOnce `--restore` and an autostarted bar's take-over running at once.
/// Every take-over and restore holds `Local\Vitrobar.Shell`, so whenever nobody holds it the
/// record exists exactly when the RunOnce value does, and with no record auto-hide is back
/// at the original.
#[test]
#[ignore = "changes the real stock taskbar"]
fn concurrent_take_over_and_restore_keep_the_invariant() {
    const ROUNDS: usize = 25;
    let original = autohide_now();
    assert!(!runonce_exists(), "a VitrobarRestore value is already set");
    let p = temp_record("race");
    let _c = Cleanup(p.clone());
    let check = |who: &str, i: usize| {
        let _lock = ShellLock::acquire(10_000).expect("shell lock");
        let record = p.exists();
        let runonce = runonce_exists();
        assert_eq!(
            record, runonce,
            "{who} round {i}: record {record}, RunOnce {runonce}"
        );
        if !record {
            assert_eq!(
                autohide_now(),
                original,
                "{who} round {i}: auto-hide left changed"
            );
        }
    };
    std::thread::scope(|s| {
        s.spawn(|| {
            for i in 0..ROUNDS {
                take_over_stock_taskbar(StockTaskbar::AutoHide, &p).unwrap();
                check("take-over", i);
            }
        });
        s.spawn(|| {
            for i in 0..ROUNDS {
                restore_stock_taskbar(&p).unwrap();
                check("restore", i);
                std::thread::sleep(Duration::from_millis(3 * (i as u64 % 4)));
            }
        });
    });
    restore_stock_taskbar(&p).unwrap();
    assert!(!p.exists());
    assert!(!runonce_exists());
    assert_eq!(autohide_now(), original);
    assert!(eventually(stock_taskbar_visible));
}
