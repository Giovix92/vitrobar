//! The file-search worker against this machine's Windows Search index. Read-only.
//! Run with `--nocapture` to see the timing and hits.

use std::time::{Duration, Instant};

use vitro_core::search::FileHit;
use vitro_win::file_search::FileSearch;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::WM_APP;

fn wait(fs: &FileSearch, limit: Duration) -> Option<(u64, Vec<FileHit>)> {
    let t = Instant::now();
    while t.elapsed() < limit {
        if let Some(r) = fs.drain() {
            return Some(r);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

#[test]
fn newest_query_wins_and_hits_are_files() {
    // No window to wake: the test polls `drain`.
    let fs = FileSearch::start(HWND::default(), WM_APP + 0x25).expect("worker");
    let t = Instant::now();
    fs.query(1, "zzzz-never-matches".into());
    fs.query(2, "desktop".into());
    let (generation, hits) = wait(&fs, Duration::from_secs(10)).expect("a result");
    println!("query 'desktop': {} hits in {:?}", hits.len(), t.elapsed());
    assert_eq!(generation, 2, "the debounce dropped the older query");
    assert!(hits.len() <= 6);
    for h in &hits {
        println!("  {} -> {}", h.name, h.path);
        assert!(std::path::Path::new(&h.path).is_file(), "{}", h.path);
        assert!(!h.name.is_empty());
    }
    assert!(
        wait(&fs, Duration::from_millis(400)).is_none(),
        "nothing else queued"
    );
    let t = Instant::now();
    drop(fs);
    assert!(t.elapsed() < Duration::from_secs(3), "drop is bounded");
}
