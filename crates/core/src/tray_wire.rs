//! Parser for the `WM_COPYDATA` payload that `Shell_NotifyIcon` sends to the taskbar.
//!
//! The payload always uses 32-bit handle fields, whatever the sender's bitness.

/// `COPYDATASTRUCT.dwData` value used by `Shell_NotifyIcon`.
pub const TRAY_COPYDATA_ID: usize = 1;
/// First `u32` of the payload.
pub const TRAY_SIGNATURE: u32 = 0x3475_3423;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayOp {
    Add,
    Modify,
    Delete,
    SetFocus,
    SetVersion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrayIconId {
    pub hwnd: u32,
    pub uid: u32,
    pub guid: Option<[u8; 16]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayMessage {
    pub op: TrayOp,
    pub id: TrayIconId,
    pub callback_msg: Option<u32>,
    pub hicon: Option<u32>,
    pub tip: Option<String>,
    pub hidden: Option<bool>,
    /// From `uVersion`; meaningful only for [`TrayOp::SetVersion`].
    pub version: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayParseError {
    TooShort,
    BadSignature,
    UnknownOp(u32),
}

const NIF_MESSAGE: u32 = 0x1;
const NIF_ICON: u32 = 0x2;
const NIF_TIP: u32 = 0x4;
const NIF_STATE: u32 = 0x8;
const NIF_GUID: u32 = 0x20;
const NIS_HIDDEN: u32 = 0x1;

// Offsets from the start of the payload (8-byte header + NOTIFYICONDATAW32).
const OFF_SIGNATURE: usize = 0;
const OFF_MESSAGE: usize = 4;
const OFF_HWND: usize = 12;
const OFF_UID: usize = 16;
const OFF_FLAGS: usize = 20;
const OFF_CALLBACK: usize = 24;
const OFF_HICON: usize = 28;
const OFF_TIP: usize = 32;
const TIP_UNITS: usize = 128;
const OFF_STATE: usize = 288;
const OFF_STATE_MASK: usize = 292;
const OFF_VERSION: usize = 808;
const OFF_GUID: usize = 944;

fn u32_at(b: &[u8], off: usize) -> Option<u32> {
    let s = b.get(off..off.checked_add(4)?)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn tip_at(b: &[u8], off: usize) -> Option<String> {
    let s = b.get(off..off + TIP_UNITS * 2)?;
    let units: Vec<u16> = (0..TIP_UNITS)
        .map(|n| u16::from_le_bytes([s[n * 2], s[n * 2 + 1]]))
        .take_while(|&u| u != 0)
        .collect();
    Some(String::from_utf16_lossy(&units))
}

/// Parses a `Shell_NotifyIcon` `WM_COPYDATA` payload. Never panics on any input.
pub fn parse_tray_copydata(bytes: &[u8]) -> Result<TrayMessage, TrayParseError> {
    let (Some(hwnd), Some(uid)) = (u32_at(bytes, OFF_HWND), u32_at(bytes, OFF_UID)) else {
        return Err(TrayParseError::TooShort);
    };
    if u32_at(bytes, OFF_SIGNATURE) != Some(TRAY_SIGNATURE) {
        return Err(TrayParseError::BadSignature);
    }
    let op = match u32_at(bytes, OFF_MESSAGE) {
        Some(0) => TrayOp::Add,
        Some(1) => TrayOp::Modify,
        Some(2) => TrayOp::Delete,
        Some(3) => TrayOp::SetFocus,
        Some(4) => TrayOp::SetVersion,
        Some(n) => return Err(TrayParseError::UnknownOp(n)),
        None => return Err(TrayParseError::TooShort),
    };
    let flags = u32_at(bytes, OFF_FLAGS).unwrap_or(0);

    let callback_msg = if flags & NIF_MESSAGE != 0 {
        u32_at(bytes, OFF_CALLBACK)
    } else {
        None
    };
    let hicon = if flags & NIF_ICON != 0 {
        u32_at(bytes, OFF_HICON)
    } else {
        None
    };
    let tip = if flags & NIF_TIP != 0 {
        tip_at(bytes, OFF_TIP)
    } else {
        None
    };
    let hidden = if flags & NIF_STATE != 0 {
        match (u32_at(bytes, OFF_STATE), u32_at(bytes, OFF_STATE_MASK)) {
            (Some(state), Some(mask)) if mask & NIS_HIDDEN != 0 => Some(state & NIS_HIDDEN != 0),
            _ => None,
        }
    } else {
        None
    };
    let guid = if flags & NIF_GUID != 0 {
        bytes
            .get(OFF_GUID..OFF_GUID + 16)
            .and_then(|s| <[u8; 16]>::try_from(s).ok())
    } else {
        None
    };
    let version = if op == TrayOp::SetVersion {
        u32_at(bytes, OFF_VERSION)
    } else {
        None
    };

    Ok(TrayMessage {
        op,
        id: TrayIconId { hwnd, uid, guid },
        callback_msg,
        hicon,
        tip,
        hidden,
        version,
    })
}

// ---- Shell_NotifyIconGetRect ----------------------------------------------------------------

/// `COPYDATASTRUCT.dwData` of the icon-rect query `Shell_NotifyIconGetRect` sends.
pub const ICON_RECT_COPYDATA_ID: usize = 3;

/// The query is made twice: once for the top-left corner, once for the bottom-right one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RectCorner {
    TopLeft,
    BottomRight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IconRectQuery {
    pub corner: RectCorner,
    pub id: TrayIconId,
}

/// Screen px: (left, top, right, bottom).
pub type IconRect = (i32, i32, i32, i32);

/// Parses the payload (`dwMagic`, `dwMessage` 1 = top-left / 2 = bottom-right, `cbSize`,
/// padding, 32-bit `hWnd`, `uID`, `guidItem`). Never panics on any input.
pub fn parse_icon_rect_query(b: &[u8]) -> Option<IconRectQuery> {
    if u32_at(b, 0)? != TRAY_SIGNATURE {
        return None;
    }
    let corner = match u32_at(b, 4)? {
        1 => RectCorner::TopLeft,
        2 => RectCorner::BottomRight,
        _ => return None,
    };
    let guid = <[u8; 16]>::try_from(b.get(24..40)?).ok()?;
    Some(IconRectQuery {
        corner,
        id: TrayIconId {
            hwnd: u32_at(b, 16)?,
            uid: u32_at(b, 20)?,
            guid: (guid != [0; 16]).then_some(guid),
        },
    })
}

/// The answer for one corner: x in the low word, y in the high word (signed 16-bit each).
pub fn icon_rect_answer(corner: RectCorner, r: IconRect) -> isize {
    let (x, y) = match corner {
        RectCorner::TopLeft => (r.0, r.1),
        RectCorner::BottomRight => (r.2, r.3),
    };
    let word = |v: i32| u32::from(v.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16 as u16);
    ((word(y) << 16) | word(x)) as i32 as isize
}

/// Where icon `id` is drawn: its slot when it is in the bar, the overflow chevron when it is
/// in the overflow, `None` when Vitrobar does not know it (Explorer answers then). A query
/// with a GUID matches by GUID, else by owner window and id.
pub fn icon_rect_for(
    id: &TrayIconId,
    promoted: &[(TrayIconId, IconRect)],
    overflowed: &[TrayIconId],
    chevron: Option<IconRect>,
) -> Option<IconRect> {
    let same = |other: &TrayIconId| match id.guid {
        Some(g) => other.guid == Some(g),
        None => other.hwnd == id.hwnd && other.uid == id.uid,
    };
    if let Some((_, r)) = promoted.iter().find(|(o, _)| same(o)) {
        return Some(*r);
    }
    overflowed.iter().any(same).then_some(chevron).flatten()
}

#[cfg(test)]
mod icon_rect_tests {
    use super::*;

    fn query(msg: u32, hwnd: u32, uid: u32, guid: [u8; 16]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend(TRAY_SIGNATURE.to_le_bytes());
        b.extend(msg.to_le_bytes());
        b.extend(40u32.to_le_bytes());
        b.extend(0u32.to_le_bytes());
        b.extend(hwnd.to_le_bytes());
        b.extend(uid.to_le_bytes());
        b.extend(guid);
        b
    }

    #[test]
    fn parses_both_corners_with_and_without_a_guid() {
        let q = parse_icon_rect_query(&query(1, 0x1234, 7, [0; 16])).unwrap();
        assert_eq!(q.corner, RectCorner::TopLeft);
        assert_eq!(
            q.id,
            TrayIconId {
                hwnd: 0x1234,
                uid: 7,
                guid: None
            }
        );
        let g = [9u8; 16];
        let q = parse_icon_rect_query(&query(2, 0, 0, g)).unwrap();
        assert_eq!(q.corner, RectCorner::BottomRight);
        assert_eq!(q.id.guid, Some(g));
    }

    #[test]
    fn junk_is_not_a_query() {
        assert_eq!(parse_icon_rect_query(&[]), None);
        assert_eq!(parse_icon_rect_query(&query(1, 1, 1, [0; 16])[..30]), None);
        assert_eq!(parse_icon_rect_query(&query(3, 1, 1, [0; 16])), None);
        let mut bad = query(1, 1, 1, [0; 16]);
        bad[0] = 0;
        assert_eq!(parse_icon_rect_query(&bad), None);
    }

    #[test]
    fn answers_pack_signed_words() {
        let r = (-1920 + 10, 1040, -1920 + 34, 1072);
        let tl = icon_rect_answer(RectCorner::TopLeft, r);
        assert_eq!(tl as u32 & 0xFFFF, (-1910i16) as u16 as u32);
        assert_eq!((tl as u32 >> 16) as i16, 1040);
        let br = icon_rect_answer(RectCorner::BottomRight, r);
        assert_eq!((br as u32 & 0xFFFF) as i16, -1886);
        assert_eq!((br as u32 >> 16) as i16, 1072);
    }

    #[test]
    fn promoted_icons_get_their_slot_and_overflowed_ones_the_chevron() {
        let a = TrayIconId {
            hwnd: 1,
            uid: 1,
            guid: None,
        };
        let g = [5u8; 16];
        let b = TrayIconId {
            hwnd: 2,
            uid: 9,
            guid: Some(g),
        };
        let c = TrayIconId {
            hwnd: 3,
            uid: 3,
            guid: None,
        };
        let promoted = [(a, (10, 0, 30, 40)), (b, (30, 0, 50, 40))];
        let chevron = Some((0, 0, 10, 40));
        assert_eq!(
            icon_rect_for(&a, &promoted, &[c], chevron),
            Some((10, 0, 30, 40))
        );
        // By GUID, whatever window the caller names.
        let by_guid = TrayIconId {
            hwnd: 0,
            uid: 0,
            guid: Some(g),
        };
        assert_eq!(
            icon_rect_for(&by_guid, &promoted, &[c], chevron),
            Some((30, 0, 50, 40))
        );
        assert_eq!(icon_rect_for(&c, &promoted, &[c], chevron), chevron);
        let unknown = TrayIconId {
            hwnd: 4,
            uid: 4,
            guid: None,
        };
        assert_eq!(icon_rect_for(&unknown, &promoted, &[c], chevron), None);
        // Overflowed but no chevron drawn (no tray bar): unknown to us.
        assert_eq!(icon_rect_for(&c, &promoted, &[c], None), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put32(buf: &mut [u8], off: usize, v: u32) {
        buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }

    /// Full-size payload (964 bytes) with the given fields.
    fn payload(op: u32, flags: u32, hwnd: u32, uid: u32) -> Vec<u8> {
        let mut b = vec![0u8; 964];
        put32(&mut b, 0, TRAY_SIGNATURE);
        put32(&mut b, 4, op);
        put32(&mut b, 8, 956);
        put32(&mut b, 12, hwnd);
        put32(&mut b, 16, uid);
        put32(&mut b, 20, flags);
        b
    }

    fn put_tip(b: &mut [u8], tip: &str) {
        for (i, u) in tip.encode_utf16().enumerate() {
            b[32 + i * 2..34 + i * 2].copy_from_slice(&u.to_le_bytes());
        }
    }

    fn payload_add(hwnd: u32, uid: u32, tip: &str) -> Vec<u8> {
        let mut b = payload(0, 0x1 | 0x2 | 0x4, hwnd, uid);
        put32(&mut b, 24, 0x8001);
        put32(&mut b, 28, 0xABCD);
        put_tip(&mut b, tip);
        b
    }

    #[test]
    fn parse_add_with_tip_and_icon() {
        let m = parse_tray_copydata(&payload_add(0x10, 7, "Hi")).unwrap();
        assert_eq!(m.op, TrayOp::Add);
        assert_eq!((m.id.hwnd, m.id.uid), (0x10, 7));
        assert_eq!(m.tip.as_deref(), Some("Hi"));
        assert_eq!(m.callback_msg, Some(0x8001));
        assert_eq!(m.hicon, Some(0xABCD));
        assert_eq!(m.hidden, None);
        assert_eq!(m.id.guid, None);
    }

    #[test]
    fn flags_gate_fields() {
        let mut b = payload_add(1, 1, "x");
        put32(&mut b, 20, 0); // no flags
        let m = parse_tray_copydata(&b).unwrap();
        assert_eq!((m.callback_msg, m.hicon, m.tip), (None, None, None));
    }

    #[test]
    fn all_ops_decode() {
        for (n, op) in [
            TrayOp::Add,
            TrayOp::Modify,
            TrayOp::Delete,
            TrayOp::SetFocus,
            TrayOp::SetVersion,
        ]
        .into_iter()
        .enumerate()
        {
            let m = parse_tray_copydata(&payload(n as u32, 0, 1, 2)).unwrap();
            assert_eq!(m.op, op);
        }
        assert_eq!(
            parse_tray_copydata(&payload(9, 0, 1, 2)),
            Err(TrayParseError::UnknownOp(9))
        );
    }

    #[test]
    fn set_version_reads_uversion() {
        let mut b = payload(4, 0, 1, 2);
        put32(&mut b, 808, 4);
        assert_eq!(parse_tray_copydata(&b).unwrap().version, Some(4));
    }

    #[test]
    fn truncated_is_error_not_panic() {
        for n in 0..20 {
            let _ = parse_tray_copydata(&payload_add(1, 1, "x")[..n]);
        }
        assert_eq!(
            parse_tray_copydata(&[0u8; 10]),
            Err(TrayParseError::TooShort)
        );
        // ends exactly before uID
        assert_eq!(
            parse_tray_copydata(&payload_add(1, 1, "x")[..16]),
            Err(TrayParseError::TooShort)
        );
        assert!(parse_tray_copydata(&payload_add(1, 1, "x")[..20]).is_ok());
    }

    #[test]
    fn bad_signature() {
        let mut b = payload_add(1, 1, "x");
        put32(&mut b, 0, 0);
        assert_eq!(parse_tray_copydata(&b), Err(TrayParseError::BadSignature));
    }

    #[test]
    fn guid_present() {
        let mut b = payload(1, 0x20, 1, 2);
        b[944..960].copy_from_slice(&[7u8; 16]);
        assert_eq!(parse_tray_copydata(&b).unwrap().id.guid, Some([7u8; 16]));
    }

    #[test]
    fn old_cbsize_without_guid() {
        let mut b = payload(1, 0x20, 1, 2);
        b.truncate(944);
        let m = parse_tray_copydata(&b).unwrap();
        assert_eq!(m.id.guid, None);
        b.truncate(100); // also cuts the tip region
        put32(&mut b, 20, 0x4 | 0x20);
        let m = parse_tray_copydata(&b).unwrap();
        assert_eq!((m.tip, m.id.guid), (None, None));
    }

    #[test]
    fn hidden_only_when_mask_set() {
        let mut b = payload(1, 0x8, 1, 2);
        put32(&mut b, 288, 1);
        assert_eq!(parse_tray_copydata(&b).unwrap().hidden, None);
        put32(&mut b, 292, 1);
        assert_eq!(parse_tray_copydata(&b).unwrap().hidden, Some(true));
        put32(&mut b, 288, 0);
        assert_eq!(parse_tray_copydata(&b).unwrap().hidden, Some(false));
    }

    #[test]
    fn tip_without_nul_uses_full_field() {
        let mut b = payload(0, 0x4, 1, 2);
        put_tip(&mut b, &"a".repeat(128));
        assert_eq!(parse_tray_copydata(&b).unwrap().tip.unwrap().len(), 128);
    }

    #[test]
    fn fuzz_random_bytes_never_panics() {
        let mut s: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        for _ in 0..10_000 {
            let len = (next() % 1100) as usize;
            let mut buf: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            // half the time make it look plausible so deep paths run
            if len >= 8 && next() % 2 == 0 {
                put32(&mut buf, 0, TRAY_SIGNATURE);
                put32(&mut buf, 4, (next() % 5) as u32);
            }
            let _ = parse_tray_copydata(&buf);
        }
    }
}
