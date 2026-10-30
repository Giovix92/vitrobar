//! Smoke tests against the real system (any Windows 11 machine has a monitor).

use vitro_win::monitors::list_monitors;
use vitro_win::system_theme::read_system_theme;

#[test]
fn exactly_one_primary_monitor_listed_first() {
    let mons = list_monitors();
    assert!(!mons.is_empty());
    assert!(mons[0].primary);
    assert_eq!(mons.iter().filter(|m| m.primary).count(), 1);
    for m in &mons {
        let (l, t, r, b) = m.rect_px;
        assert!(r > l && b > t, "{m:?}");
        assert!(m.dpi >= 96, "{m:?}");
    }
}

#[test]
fn system_theme_reads() {
    let t = read_system_theme();
    assert!(t.accent <= 0xFF_FFFF);
}
