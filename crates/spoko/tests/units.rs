use spoko::commands::project::parse_at;
use spoko::commands::{fmt_size, fmt_time};
use spoko_client::daemon::ipc::RestoreSpec;

#[test]
fn at_parsing() {
    let oid = "0123456789abcdef0123456789abcdef01234567";
    assert_eq!(parse_at(oid, 0).unwrap(), RestoreSpec::Commit { oid: oid.into() });
    assert_eq!(parse_at("1700000000", 0).unwrap(), RestoreSpec::Before { time: 1_700_000_000 });
    assert_eq!(parse_at("10m", 10_000).unwrap(), RestoreSpec::Before { time: 9_400 });
    assert_eq!(parse_at("2h", 10_000).unwrap(), RestoreSpec::Before { time: 2_800 });
    assert_eq!(parse_at("1d", 100_000).unwrap(), RestoreSpec::Before { time: 13_600 });
    assert_eq!(parse_at("5s", 10).unwrap(), RestoreSpec::Before { time: 5 });
    assert!(parse_at("yesterday", 0).is_err());
    assert!(parse_at("xm", 0).is_err());
}

#[test]
fn formatting() {
    assert_eq!(fmt_time(0), "1970-01-01 00:00:00 UTC");
    assert_eq!(fmt_time(1_793_271_900), "2026-10-29 11:05:00 UTC");
    assert_eq!(fmt_time(951_782_400), "2000-02-29 00:00:00 UTC");
    assert_eq!(fmt_size(999), "999 B");
    assert_eq!(fmt_size(1536), "1.5 KiB");
    assert_eq!(fmt_size(5 << 30), "5.0 GiB");
}
