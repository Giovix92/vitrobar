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
    pub fn remove_owner(&mut self, hwnd: u32) -> usize {
        let before = self.icons.len();
        self.icons.retain(|i| i.id.hwnd != hwnd);
        before - self.icons.len()
    }
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
}
