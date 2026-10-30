//! Read-only smoke tests of the first-run import against this machine's real settings.
//! Run with `--nocapture` to see what would be imported.

use vitro_win::pins_import::{import_promoted_tray, import_windows_pins};
use vitro_win::system_theme::user_ui_language_tag;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};

#[test]
fn imports_read_without_failing() {
    // SAFETY: per-thread apartment for the shortcut COM objects, balanced below.
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
    let pins = import_windows_pins();
    for p in &pins {
        assert!(!p.name.is_empty());
        assert!(p.aumid.is_some() || p.path.is_some(), "{p:?}");
        println!(
            "pin {:?} aumid={:?} path={:?} link={:?}",
            p.name, p.aumid, p.path, p.link
        );
    }
    for k in import_promoted_tray() {
        assert!(k.starts_with('{') || k.contains('|'), "{k}");
        println!("promoted {k}");
    }
    if com {
        // SAFETY: balances the successful CoInitializeEx above.
        unsafe { CoUninitialize() };
    }
}

#[test]
fn ui_language_tag_reads() {
    let tag = user_ui_language_tag().expect("display language");
    assert!(tag.len() >= 2 && tag.is_ascii(), "{tag}");
    println!("ui language {tag}");
}
