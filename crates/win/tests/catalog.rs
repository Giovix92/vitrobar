//! Read-only checks of the Start panel's app catalog and recent files against this machine.
//! Run with `--nocapture` to see counts and timings.

use std::collections::HashSet;
use std::path::Path;
use std::time::Instant;

use vitro_core::search::fold;
use vitro_win::catalog::{list_all_apps, recent_files};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};

/// COM STA for the test thread, released on drop.
struct Com(bool);

impl Com {
    fn init() -> Self {
        // SAFETY: per-thread apartment, balanced in Drop when it succeeded.
        Self(unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok())
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances the successful CoInitializeEx.
            unsafe { CoUninitialize() };
        }
    }
}

#[test]
fn all_apps_contains_settings_and_explorer() {
    let _com = Com::init();
    let t = Instant::now();
    let apps = list_all_apps();
    println!("list_all_apps: {} apps in {:?}", apps.len(), t.elapsed());
    let aumids: Vec<String> = apps
        .iter()
        .map(|p| {
            p.aumid
                .clone()
                .expect("every catalog entry has an aumid")
                .to_lowercase()
        })
        .collect();
    for want in [
        "windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel",
        "microsoft.windows.explorer",
    ] {
        assert!(aumids.iter().any(|a| a == want), "{want} missing");
    }
    let unique: HashSet<&String> = aumids.iter().collect();
    assert_eq!(unique.len(), aumids.len(), "duplicate aumids");
    for w in apps.windows(2) {
        assert!(
            fold(&w[0].name) <= fold(&w[1].name),
            "{:?} before {:?}",
            w[0].name,
            w[1].name
        );
    }
    for p in &apps {
        assert!(!p.name.is_empty());
        let n = p.name.to_lowercase();
        assert!(
            !n.starts_with("uninstall") && !n.contains("readme"),
            "{}",
            p.name
        );
        let a = p.aumid.as_deref().unwrap().to_lowercase();
        assert!(
            !a.starts_with("http:") && !a.starts_with("https:") && !a.starts_with("ms-"),
            "{a}"
        );
    }
}

#[test]
fn recent_files_are_existing_files() {
    let _com = Com::init();
    for max in [0, 3, 20] {
        let t = Instant::now();
        let files = recent_files(max);
        println!("recent_files({max}): {} in {:?}", files.len(), t.elapsed());
        assert!(files.len() <= max);
        for f in &files {
            let p = Path::new(&f.path);
            assert!(p.is_file(), "{}", f.path);
            assert_eq!(
                p.file_name().unwrap().to_string_lossy(),
                f.name.as_str(),
                "name is the target's file name"
            );
        }
    }
}
