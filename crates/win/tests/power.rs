//! The only power test: it runs in dry-run mode. Never call `perform` without dry run here.

use vitro_win::power::{last_dry_run, perform, set_dry_run, PowerAction};

#[test]
fn dry_run_records_and_does_nothing() {
    set_dry_run(true);
    assert_eq!(last_dry_run(), None);
    perform(PowerAction::ShutDown).expect("dry run succeeds");
    assert_eq!(last_dry_run(), Some(PowerAction::ShutDown));
}
