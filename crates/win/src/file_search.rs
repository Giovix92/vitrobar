//! Start panel file search: a worker thread ("vitrobar-search", COM STA) that asks the
//! Windows Search index for files under the user's profile whose name contains the text.
//! Nothing is indexed here: the query is a shell search folder (`ISearchFolderItemFactory`)
//! enumerated like any other folder. [`search_uri`] gives the same search as a `search-ms:`
//! URI, for opening it in Explorer.
//!
//! The UI thread sends `(generation, text)` with [`FileSearch::query`]; the worker waits
//! [`DEBOUNCE`] for the typing to settle (a newer query replaces the pending one, and one
//! arriving mid-enumeration cancels it), then enumerates at most [`MAX_HITS`] files or for
//! [`QUERY_BUDGET`]. Results travel in a [`WakeQueue`]; the window message only wakes the UI
//! thread, which then calls [`FileSearch::drain`].

use std::os::windows::fs::MetadataExt;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use vitro_core::search::FileHit;
use windows::core::{Error, Result, HSTRING, PCWSTR};
use windows::Win32::Foundation::{E_FAIL, HWND, LPARAM, WPARAM};
use windows::Win32::Storage::EnhancedStorage::PKEY_ItemNameDisplay;
use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_SYSTEM};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, IBindCtx, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Search::Common::COP_VALUE_CONTAINS;
use windows::Win32::System::Search::{
    ConditionFactory, ICondition, IConditionFactory2, CONDITION_CREATION_DEFAULT,
};
use windows::Win32::UI::Shell::{
    BHID_EnumItems, IEnumShellItems, ISearchFolderItemFactory, IShellItem, IShellItemArray,
    SHCreateItemFromParsingName, SHCreateShellItemArrayFromShellItem, SearchFolderItemFactory,
    SIGDN_FILESYSPATH, SIGDN_NORMALDISPLAY,
};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::catalog::item_name;
use crate::event_queue::WakeQueue;

/// How long the worker waits for typing to settle before querying.
pub const DEBOUNCE: Duration = Duration::from_millis(120);
/// Files per query.
pub const MAX_HITS: usize = 6;
/// Time per query; the enumeration stops with what it has.
pub const QUERY_BUDGET: Duration = Duration::from_secs(1);
/// How long dropping a [`FileSearch`] waits for its worker.
const JOIN_BOUND: Duration = Duration::from_secs(2);

type Query = (u64, String);
type Answer = (u64, Vec<FileHit>);

fn unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~')
}

/// Percent-encodes every UTF-8 byte outside `[A-Za-z0-9-_.~]`.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if unreserved(b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn search_uri_in(text: &str, location: &str) -> String {
    format!(
        "search-ms:query={}&crumb=location:{}",
        percent_encode(text),
        percent_encode(location)
    )
}

/// The `search-ms:` URI of an Explorer search for `text` under `%USERPROFILE%` (for a
/// "more results" link, opened with [`crate::launch::shell_open`]).
pub fn search_uri(text: &str) -> String {
    let profile = std::env::var("USERPROFILE").unwrap_or_default();
    search_uri_in(text, &profile)
}

/// The file-search worker. Dropping it stops the worker (waiting at most 2 s for it).
pub struct FileSearch {
    tx: Option<Sender<Query>>,
    results: Arc<WakeQueue<Answer>>,
    /// Disconnects when the worker ends.
    done: Receiver<()>,
    thread: Option<JoinHandle<()>>,
}

impl FileSearch {
    /// Starts the worker. Each finished query posts `wake_msg` (no payload) to `notify`
    /// unless a wake-up is already pending; a null `notify` posts nothing.
    pub fn start(notify: HWND, wake_msg: u32) -> Result<Self> {
        let (tx, rx) = mpsc::channel::<Query>();
        let (done_tx, done) = mpsc::channel::<()>();
        let results = Arc::new(WakeQueue::new());
        let queue = Arc::clone(&results);
        // HWND is a raw pointer (not Send): carry the value.
        let notify = notify.0 as isize;
        let thread = std::thread::Builder::new()
            .name("vitrobar-search".into())
            .spawn(move || {
                let _done = done_tx;
                worker(&rx, &queue, notify, wake_msg);
            })
            .map_err(|e| Error::new(E_FAIL, e.to_string()))?;
        Ok(Self {
            tx: Some(tx),
            results,
            done,
            thread: Some(thread),
        })
    }

    /// Asks for the files matching `text`; replaces any query not yet finished. Blank text
    /// answers at once with no hits.
    pub fn query(&self, generation: u64, text: String) {
        if let Some(tx) = &self.tx {
            let _ = tx.send((generation, text));
        }
    }

    /// The most recent finished result, discarding older ones; `None` if nothing finished
    /// since the last call.
    pub fn drain(&self) -> Option<(u64, Vec<FileHit>)> {
        self.results.drain().into_iter().max_by_key(|(g, _)| *g)
    }
}

impl Drop for FileSearch {
    fn drop(&mut self) {
        drop(self.tx.take());
        // The worker notices the closed channel between items; one blocked inside the
        // shell is left to finish on its own after the bound.
        if let Err(RecvTimeoutError::Disconnected) = self.done.recv_timeout(JOIN_BOUND) {
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }
}

/// COM STA for the worker thread, released on drop.
struct Apartment(bool);

impl Apartment {
    fn enter() -> Self {
        // SAFETY: initialises COM for this (worker) thread; balanced in Drop.
        Self(unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok())
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances the successful CoInitializeEx on this thread.
            unsafe { CoUninitialize() };
        }
    }
}

enum Outcome {
    Done(Vec<FileHit>),
    /// A newer query arrived; this one was abandoned.
    Newer(Query),
    Closed,
}

fn worker(rx: &Receiver<Query>, queue: &WakeQueue<Answer>, notify: isize, wake_msg: u32) {
    let _com = Apartment::enter();
    let mut next: Option<Query> = None;
    loop {
        let mut pending = match next.take() {
            Some(q) => q,
            None => match rx.recv() {
                Ok(q) => q,
                Err(_) => return,
            },
        };
        // Debounce: wait for the typing to settle.
        loop {
            match rx.recv_timeout(DEBOUNCE) {
                Ok(q) => pending = q,
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        let (generation, text) = pending;
        let hits = match run_query(&text, rx) {
            Outcome::Done(hits) => hits,
            Outcome::Newer(q) => {
                next = Some(q);
                continue;
            }
            Outcome::Closed => return,
        };
        if queue.push((generation, hits)) && notify != 0 {
            // SAFETY: posting never blocks; a stale handle just fails.
            let posted = unsafe {
                PostMessageW(Some(HWND(notify as *mut _)), wake_msg, WPARAM(0), LPARAM(0))
            };
            if posted.is_err() {
                queue.wake_failed();
            }
        }
    }
}

/// Enumerates the search folder for `text`, checking `rx` between items.
fn run_query(text: &str, rx: &Receiver<Query>) -> Outcome {
    let text = text.trim();
    if text.is_empty() {
        return Outcome::Done(Vec::new());
    }
    let deadline = Instant::now() + QUERY_BUDGET;
    let Some(items) = enum_search(text) else {
        return Outcome::Done(Vec::new());
    };
    let mut hits = Vec::new();
    while hits.len() < MAX_HITS && Instant::now() < deadline {
        match rx.try_recv() {
            Ok(q) => return Outcome::Newer(q),
            Err(TryRecvError::Disconnected) => return Outcome::Closed,
            Err(TryRecvError::Empty) => {}
        }
        let mut one: [Option<IShellItem>; 1] = [None];
        let mut fetched = 0u32;
        // SAFETY: `one` and `fetched` are valid out-parameters for one item.
        let r = unsafe { items.Next(&mut one, Some(&mut fetched)) };
        if r.is_err() || fetched == 0 {
            break;
        }
        let Some(item) = one[0].take() else { break };
        if let Some(hit) = file_hit(&item) {
            hits.push(hit);
        }
    }
    Outcome::Done(hits)
}

/// The Windows Search folder of items under `%USERPROFILE%` whose display name contains
/// `text`, as an enumerator. (A parsed `search-ms:` URI binds but fails to enumerate outside
/// Explorer with `E_UNEXPECTED`; the search folder factory is the documented route.)
fn enum_search(text: &str) -> Option<IEnumShellItems> {
    let profile = HSTRING::from(std::env::var_os("USERPROFILE")?);
    let value = HSTRING::from(text);
    // SAFETY: in-process COM objects on this STA thread; `profile` and `value` outlive the
    // calls that read them.
    unsafe {
        let factory: ISearchFolderItemFactory =
            CoCreateInstance(&SearchFolderItemFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let scope: IShellItem =
            SHCreateItemFromParsingName(PCWSTR(profile.as_ptr()), None::<&IBindCtx>).ok()?;
        let scope: IShellItemArray = SHCreateShellItemArrayFromShellItem(&scope).ok()?;
        factory.SetScope(&scope).ok()?;
        let conditions: IConditionFactory2 =
            CoCreateInstance(&ConditionFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let condition: ICondition = conditions
            .CreateStringLeaf(
                &PKEY_ItemNameDisplay,
                COP_VALUE_CONTAINS,
                &value,
                PCWSTR::null(),
                CONDITION_CREATION_DEFAULT,
            )
            .ok()?;
        factory.SetCondition(&condition).ok()?;
        let folder: IShellItem = factory.GetShellItem().ok()?;
        folder
            .BindToHandler(None::<&IBindCtx>, &BHID_EnumItems)
            .ok()
    }
}

/// Whether `path` lies in the profile's `AppData` (app state, not the user's documents).
fn in_app_data(path: &str, profile: &str) -> bool {
    let root = format!("{}\\appdata\\", profile.trim_end_matches('\\')).to_lowercase();
    path.to_lowercase().starts_with(&root)
}

/// A search result that is an existing regular file the user would look for: folders,
/// items without a file system path, hidden or system files and anything in `AppData` are
/// skipped. A `.zip`, which the shell treats as a folder, counts as a file.
fn file_hit(item: &IShellItem) -> Option<FileHit> {
    let path = item_name(item, SIGDN_FILESYSPATH)?;
    let profile = std::env::var("USERPROFILE").unwrap_or_default();
    if !profile.is_empty() && in_app_data(&path, &profile) {
        return None;
    }
    let p = Path::new(&path);
    let meta = std::fs::metadata(p).ok()?;
    let hidden = FILE_ATTRIBUTE_HIDDEN.0 | FILE_ATTRIBUTE_SYSTEM.0;
    if !meta.is_file() || meta.file_attributes() & hidden != 0 {
        return None;
    }
    let name = item_name(item, SIGDN_NORMALDISPLAY)
        .or_else(|| Some(p.file_name()?.to_string_lossy().into_owned()))?;
    Some(FileHit { name, path })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_uri_escapes_everything_outside_unreserved() {
        assert_eq!(percent_encode("aZ09-_.~"), "aZ09-_.~");
        assert_eq!(percent_encode("a b"), "a%20b");
        assert_eq!(percent_encode("a&b=c"), "a%26b%3Dc");
        assert_eq!(percent_encode("100%"), "100%25");
        assert_eq!(percent_encode("\"x\" 'y'"), "%22x%22%20%27y%27");
        assert_eq!(percent_encode("è"), "%C3%A8");
        assert_eq!(percent_encode("日本"), "%E6%97%A5%E6%9C%AC");
        assert_eq!(percent_encode(r"C:\Users\Ann"), "C%3A%5CUsers%5CAnn");
        assert_eq!(
            search_uri_in("tax & fees", r"C:\Users\Ann"),
            "search-ms:query=tax%20%26%20fees&crumb=location:C%3A%5CUsers%5CAnn"
        );
    }

    #[test]
    fn app_data_is_not_a_place_for_documents() {
        let profile = r"C:\Users\Ann";
        assert!(in_app_data(r"C:\Users\Ann\AppData\Local\x.txt", profile));
        assert!(in_app_data(
            r"c:\users\ann\appdata\Roaming\x.txt",
            r"C:\Users\Ann\"
        ));
        assert!(!in_app_data(r"C:\Users\Ann\Documents\AppData.txt", profile));
        assert!(!in_app_data(r"C:\Users\Ann\AppDataX\x.txt", profile));
    }

    #[test]
    fn search_uri_uses_the_user_profile() {
        let profile = std::env::var("USERPROFILE").unwrap();
        assert_eq!(search_uri("x"), search_uri_in("x", &profile));
        assert!(search_uri("x").starts_with("search-ms:query=x&crumb=location:"));
    }
}
