//! The `WM_COPYDATA` payload that `SHAppBarMessage` sends to the taskbar (`dwData` 0).
//!
//! shell32's `TRAYAPPBARDATA`, the same on every platform (handle fields are 64-bit wide):
//!
//! | offset | field |
//! |---|---|
//! | 0 | `APPBARDATA3264` (40 bytes: cbSize, hWnd, uCallbackMessage, uEdge, rc, lParam) |
//! | 40 | `dwMessage` (`ABM_*`) |
//! | 48 | `hSharedMem`: the shared block holding the caller's `APPBARDATA3264`, or 0 |
//! | 56 | `dwProcId`: the process in which `hSharedMem` is valid |
//!
//! For `ABM_QUERYPOS` and `ABM_SETPOS` (and the other calls that return a rectangle) the
//! taskbar reads the request from the shared block and writes its answer back into it. The
//! block is allocated for the process that owns the `Shell_TrayWnd` the caller found, so
//! `dwProcId` names the receiver, not the sender.

/// `COPYDATASTRUCT.dwData` value used by `SHAppBarMessage`.
pub const APPBAR_COPYDATA_ID: usize = 0;
/// Size of the whole payload.
pub const TRAY_APPBAR_DATA_SIZE: usize = 64;
/// Size of the `APPBARDATA3264` held in the shared block (also its own `cbSize`).
pub const APPBAR_DATA_SIZE: usize = 40;

const MESSAGE: usize = 40;
const SHARED: usize = 48;
const PROC: usize = 56;

/// The fields of one `SHAppBarMessage` payload that a relay needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppBarCall {
    /// `ABM_*`.
    pub message: u32,
    /// The shared block's handle (0: the call carries no block).
    pub shared: u64,
    /// The process in which [`AppBarCall::shared`] is valid.
    pub proc_id: u32,
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

/// Reads an `SHAppBarMessage` payload; `None` if it is shorter than `TRAYAPPBARDATA`.
pub fn parse_appbar_copydata(bytes: &[u8]) -> Option<AppBarCall> {
    if bytes.len() < TRAY_APPBAR_DATA_SIZE {
        return None;
    }
    let mut shared = [0u8; 8];
    shared.copy_from_slice(&bytes[SHARED..SHARED + 8]);
    Some(AppBarCall {
        message: u32_at(bytes, MESSAGE),
        shared: u64::from_le_bytes(shared),
        proc_id: u32_at(bytes, PROC),
    })
}

/// A copy of `bytes` whose shared block is `shared`, valid in process `proc_id`; the rest of
/// the payload (and any bytes past `TRAYAPPBARDATA`) is kept as it is.
pub fn with_shared_block(bytes: &[u8], shared: u64, proc_id: u32) -> Option<Vec<u8>> {
    if bytes.len() < TRAY_APPBAR_DATA_SIZE {
        return None;
    }
    let mut out = bytes.to_vec();
    out[SHARED..SHARED + 8].copy_from_slice(&shared.to_le_bytes());
    out[PROC..PROC + 4].copy_from_slice(&proc_id.to_le_bytes());
    Some(out)
}

/// The `APPBARDATA3264` in a shared block looks like one (its `cbSize` is 40).
pub fn is_appbar_data(block: &[u8]) -> bool {
    block.len() >= APPBAR_DATA_SIZE && u32_at(block, 0) as usize == APPBAR_DATA_SIZE
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The payload seen live from `SHAppBarMessage(ABM_QUERYPOS)` (bottom edge, 0,1000 to
    /// 1920,1080) when our `Shell_TrayWnd` is the one found.
    fn querypos() -> Vec<u8> {
        let mut b = vec![0u8; 64];
        b[0] = 0x28;
        b[4..8].copy_from_slice(&0x0046_0dbcu32.to_le_bytes());
        b[8..12].copy_from_slice(&0x8030u32.to_le_bytes());
        b[12] = 3;
        b[20..24].copy_from_slice(&1000u32.to_le_bytes());
        b[24..28].copy_from_slice(&1920u32.to_le_bytes());
        b[28..32].copy_from_slice(&1080u32.to_le_bytes());
        b[40] = 2;
        b[48..56].copy_from_slice(&0x01d1_eacc_a8c0u64.to_le_bytes());
        b[56..60].copy_from_slice(&0x7b58u32.to_le_bytes());
        b
    }

    #[test]
    fn reads_message_block_and_process() {
        assert_eq!(
            parse_appbar_copydata(&querypos()),
            Some(AppBarCall {
                message: 2,
                shared: 0x01d1_eacc_a8c0,
                proc_id: 0x7b58,
            })
        );
        assert_eq!(parse_appbar_copydata(&[0u8; 63]), None);
    }

    #[test]
    fn swaps_only_block_and_process() {
        let src = querypos();
        let out = with_shared_block(&src, 0xc0b4, 0x399c).unwrap();
        assert_eq!(out.len(), src.len());
        assert_eq!(out[..48], src[..48]);
        assert_eq!(out[60..], src[60..]);
        let call = parse_appbar_copydata(&out).unwrap();
        assert_eq!(
            (call.message, call.shared, call.proc_id),
            (2, 0xc0b4, 0x399c)
        );
        // Longer payloads (a newer shell32) keep their tail.
        let mut long = src.clone();
        long.extend_from_slice(&[7, 8, 9]);
        assert_eq!(with_shared_block(&long, 1, 2).unwrap()[64..], [7, 8, 9]);
        assert_eq!(with_shared_block(&[0u8; 10], 1, 2), None);
    }

    #[test]
    fn block_must_be_appbardata3264() {
        assert!(is_appbar_data(&querypos()[..40]));
        assert!(!is_appbar_data(&[0u8; 40]));
        assert!(!is_appbar_data(&querypos()[..39]));
    }
}
