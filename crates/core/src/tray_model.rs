//! Icon registry fed by parsed tray messages. Matches Explorer's identity rules.

use crate::tray_wire::{TrayIconId, TrayMessage, TrayOp};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayIcon {
    pub id: TrayIconId,
    pub callback_msg: u32,
    pub hicon: u32,
    pub tip: String,
    pub hidden: bool,
    pub version: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrayModel {
    icons: Vec<TrayIcon>,
}

impl TrayModel {
    pub fn new() -> Self {
        Self::default()
    }
    fn position(&self, id: &TrayIconId) -> Option<usize> {
        self.icons
            .iter()
            .position(|i| match (&i.id.guid, &id.guid) {
                (Some(a), Some(b)) => a == b,
                _ => i.id.hwnd == id.hwnd && i.id.uid == id.uid,
            })
    }

    /// Applies one message; returns whether the model changed or the message was valid for it.
    pub fn apply(&mut self, m: TrayMessage) -> bool {
        let pos = self.position(&m.id);
        match (m.op, pos) {
            (TrayOp::Add, Some(_)) => false,
            (TrayOp::Add, None) => {
                self.icons.push(TrayIcon {
                    id: m.id,
                    callback_msg: m.callback_msg.unwrap_or(0),
                    hicon: m.hicon.unwrap_or(0),
                    tip: m.tip.unwrap_or_default(),
                    hidden: m.hidden.unwrap_or(false),
                    version: 0,
                });
                true
            }
            (_, None) => false,
            (TrayOp::Modify, Some(i)) => {
                let icon = &mut self.icons[i];
                if m.id.guid.is_some() && icon.id.guid == m.id.guid {
                    // Matched by guid: the owner window (and uid) may have changed, and
                    // `remove_owner` must follow the live one.
                    icon.id.hwnd = m.id.hwnd;
                    icon.id.uid = m.id.uid;
                }
                if let Some(v) = m.callback_msg {
                    icon.callback_msg = v;
                }
                if let Some(v) = m.hicon {
                    icon.hicon = v;
                }
                if let Some(v) = m.tip {
                    icon.tip = v;
                }
                if let Some(v) = m.hidden {
                    icon.hidden = v;
                }
                true
            }
            (TrayOp::Delete, Some(i)) => {
                self.icons.remove(i);
                true
            }
            (TrayOp::SetFocus, Some(_)) => true,
            (TrayOp::SetVersion, Some(i)) => {
                self.icons[i].version = m.version.unwrap_or(0);
                true
            }
        }
    }
    pub fn icons(&self) -> &[TrayIcon] {
        &self.icons
    }
    /// Drops every icon owned by `hwnd` (its window died); returns how many.
    ///
    /// Owners are the windows of the last Add or guid-matched Modify.
    pub fn remove_owner(&mut self, hwnd: u32) -> usize {
        let before = self.icons.len();
        self.icons.retain(|i| i.id.hwnd != hwnd);
        before - self.icons.len()
    }
}

/// `{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}` in lowercase hex, from a GUID's 16 bytes as laid
/// out in memory (`Data1`..`Data3` little-endian).
pub fn guid_string(g: &[u8; 16]) -> String {
    let d1 = u32::from_le_bytes([g[0], g[1], g[2], g[3]]);
    let d2 = u16::from_le_bytes([g[4], g[5]]);
    let d3 = u16::from_le_bytes([g[6], g[7]]);
    let tail: String = g[10..].iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{{{d1:08x}-{d2:04x}-{d3:04x}-{:02x}{:02x}-{tail}}}",
        g[8], g[9]
    )
}

/// The stable identity of a tray icon across restarts (what `Settings.tray_promoted`
/// stores): its GUID when it has one, else `"<exe path lowercase>|<uid>"`.
pub fn tray_key(guid: Option<&[u8; 16]>, exe_path: &str, uid: u32) -> String {
    match guid {
        Some(g) => guid_string(g),
        None => format!("{}|{uid}", exe_path.to_lowercase()),
    }
}

/// Splits the visible icons into the bar (keys listed in `promoted`, in model order) and
/// the overflow (the rest). Hidden icons are in neither. Returns indices into `icons`.
pub fn split_promoted(
    icons: &[TrayIcon],
    keys: &[String],
    promoted: &[String],
) -> (Vec<usize>, Vec<usize>) {
    let (mut bar, mut overflow) = (Vec::new(), Vec::new());
    for (i, icon) in icons.iter().enumerate() {
        if icon.hidden {
            continue;
        }
        if keys.get(i).is_some_and(|k| promoted.contains(k)) {
            bar.push(i);
        } else {
            overflow.push(i);
        }
    }
    (bar, overflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(hwnd: u32, uid: u32) -> TrayIconId {
        TrayIconId {
            hwnd,
            uid,
            guid: None,
        }
    }
    fn msg(op: TrayOp, id: TrayIconId) -> TrayMessage {
        TrayMessage {
            op,
            id,
            callback_msg: None,
            hicon: None,
            tip: None,
            hidden: None,
            version: None,
        }
    }
    fn add(i: TrayIconId, tip: &str) -> TrayMessage {
        TrayMessage {
            callback_msg: Some(0x400),
            hicon: Some(9),
            tip: Some(tip.into()),
            ..msg(TrayOp::Add, i)
        }
    }

    #[test]
    fn model_add_modify_delete() {
        let mut m = TrayModel::new();
        assert!(m.apply(add(id(1, 1), "a")));
        assert_eq!(m.icons().len(), 1);
        assert_eq!(m.icons()[0].version, 0);
        let mut md = msg(TrayOp::Modify, id(1, 1));
        md.tip = Some("b".into());
        assert!(m.apply(md));
        assert_eq!(m.icons()[0].tip, "b");
        assert_eq!(m.icons()[0].hicon, 9);
        assert_eq!(m.icons()[0].callback_msg, 0x400);
        assert!(m.apply(msg(TrayOp::Delete, id(1, 1))));
        assert!(m.icons().is_empty());
    }

    #[test]
    fn duplicate_add_rejected() {
        let mut m = TrayModel::new();
        assert!(m.apply(add(id(1, 1), "a")));
        assert!(!m.apply(add(id(1, 1), "z")));
        assert_eq!(m.icons().len(), 1);
        assert_eq!(m.icons()[0].tip, "a");
    }

    #[test]
    fn unknown_id_operations_fail() {
        let mut m = TrayModel::new();
        assert!(!m.apply(msg(TrayOp::Modify, id(1, 1))));
        assert!(!m.apply(msg(TrayOp::Delete, id(1, 1))));
        assert!(!m.apply(msg(TrayOp::SetVersion, id(1, 1))));
    }

    #[test]
    fn set_focus_true_no_change() {
        let mut m = TrayModel::new();
        m.apply(add(id(1, 1), "a"));
        let before = m.clone();
        assert!(m.apply(msg(TrayOp::SetFocus, id(1, 1))));
        assert_eq!(m, before);
    }

    #[test]
    fn set_version_updates() {
        let mut m = TrayModel::new();
        m.apply(add(id(1, 1), "a"));
        let mut sv = msg(TrayOp::SetVersion, id(1, 1));
        sv.version = Some(4);
        assert!(m.apply(sv));
        assert_eq!(m.icons()[0].version, 4);
    }

    #[test]
    fn insertion_order_kept() {
        let mut m = TrayModel::new();
        m.apply(add(id(1, 1), "a"));
        m.apply(add(id(1, 2), "b"));
        m.apply(add(id(1, 3), "c"));
        m.apply(msg(TrayOp::Delete, id(1, 2)));
        let tips: Vec<_> = m.icons().iter().map(|i| i.tip.as_str()).collect();
        assert_eq!(tips, ["a", "c"]);
    }

    #[test]
    fn guid_identity_wins() {
        let g = Some([3u8; 16]);
        let mut m = TrayModel::new();
        assert!(m.apply(add(
            TrayIconId {
                hwnd: 1,
                uid: 1,
                guid: g
            },
            "a"
        )));
        let mut md = msg(
            TrayOp::Modify,
            TrayIconId {
                hwnd: 2,
                uid: 9,
                guid: g,
            },
        );
        md.tip = Some("b".into());
        assert!(m.apply(md));
        assert_eq!(m.icons().len(), 1);
        assert_eq!(m.icons()[0].tip, "b");
    }

    #[test]
    fn hidden_state() {
        let mut m = TrayModel::new();
        m.apply(add(id(1, 1), "a"));
        let mut md = msg(TrayOp::Modify, id(1, 1));
        md.hidden = Some(true);
        assert!(m.apply(md));
        assert!(m.icons()[0].hidden);
    }

    #[test]
    fn remove_owner_drops_all_of_hwnd() {
        let mut m = TrayModel::new();
        m.apply(add(id(5, 1), "a"));
        m.apply(add(id(5, 2), "b"));
        m.apply(add(id(6, 1), "c"));
        assert_eq!(m.remove_owner(5), 2);
        assert_eq!(m.icons().len(), 1);
        assert_eq!(m.icons()[0].id.hwnd, 6);
        assert_eq!(m.remove_owner(5), 0);
    }

    #[test]
    fn guid_modify_moves_the_owner() {
        let g = Some([3u8; 16]);
        let mut m = TrayModel::new();
        m.apply(add(
            TrayIconId {
                hwnd: 1,
                uid: 1,
                guid: g,
            },
            "a",
        ));
        let md = msg(
            TrayOp::Modify,
            TrayIconId {
                hwnd: 2,
                uid: 9,
                guid: g,
            },
        );
        assert!(m.apply(md));
        assert_eq!((m.icons()[0].id.hwnd, m.icons()[0].id.uid), (2, 9));
        // The old owner dying must not drop the live icon; the new one dying must.
        assert_eq!(m.remove_owner(1), 0);
        assert_eq!(m.icons().len(), 1);
        assert_eq!(m.remove_owner(2), 1);
        assert!(m.icons().is_empty());
    }

    #[test]
    fn plain_modify_keeps_the_owner() {
        let mut m = TrayModel::new();
        m.apply(add(id(4, 2), "a"));
        let mut md = msg(TrayOp::Modify, id(4, 2));
        md.tip = Some("b".into());
        m.apply(md);
        assert_eq!((m.icons()[0].id.hwnd, m.icons()[0].id.uid), (4, 2));
    }

    #[test]
    fn keys_use_guid_else_exe_and_uid() {
        // {6b29fc40-ca47-1067-b31d-00dd010662da} in memory order.
        let g = [
            0x40, 0xfc, 0x29, 0x6b, 0x47, 0xca, 0x67, 0x10, 0xb3, 0x1d, 0x00, 0xdd, 0x01, 0x06,
            0x62, 0xda,
        ];
        assert_eq!(
            tray_key(Some(&g), "ignored", 1),
            "{6b29fc40-ca47-1067-b31d-00dd010662da}"
        );
        assert_eq!(
            tray_key(None, r"C:\Program Files\App\App.EXE", 7),
            r"c:\program files\app\app.exe|7"
        );
    }

    #[test]
    fn split_keeps_order_and_skips_hidden() {
        let mut m = TrayModel::new();
        m.apply(add(id(1, 1), "a"));
        m.apply(add(id(1, 2), "b"));
        m.apply(add(id(1, 3), "c"));
        let mut hide = msg(TrayOp::Modify, id(1, 3));
        hide.hidden = Some(true);
        m.apply(hide);
        let keys: Vec<String> = ["ka", "kb", "kc"].map(String::from).to_vec();
        let (bar, over) = split_promoted(m.icons(), &keys, &["kb".into(), "kc".into()]);
        assert_eq!(bar, vec![1]);
        assert_eq!(over, vec![0]);
        let (bar, over) = split_promoted(m.icons(), &keys, &[]);
        assert!(bar.is_empty());
        assert_eq!(over, vec![0, 1]);
    }
}
