//! Files sealed by other implementations of drand's timelock must open here.

use ignot_core::drand::Beacon;
use ignot_core::timelock::{open_bytes, seal_bytes, sealed_round};

/// quicknet's signature of round 1000, as every relay serves it.
const SIGNATURE: &str =
    "b44679b9a59af2ec876b1a6b1ad52ea9b1615fc3982b19576350f93447cb1125e342b73a8dd2bacbe47e4b6b63ed5e39";

/// Sealed with tlock-js 0.9.0, the JavaScript implementation the drand project maintains.
const FROM_TLOCK_JS: &str = include_str!("data/tlock-js-round-1000.age");

#[test]
fn a_file_sealed_by_tlock_js_opens_here() -> ignot_core::Result<()> {
    assert_eq!(sealed_round(FROM_TLOCK_JS.as_bytes())?, 1000);
    let beacon = Beacon::parse(1000, SIGNATURE)?;
    let expected =
        "Sealed by tlock-js 0.9.0, the JavaScript timelock tool of the drand project, to quicknet round 1000.\n";
    assert_eq!(open_bytes(FROM_TLOCK_JS.as_bytes(), &beacon)?, expected.as_bytes());
    Ok(())
}

#[test]
fn the_public_api_seals_and_opens() -> ignot_core::Result<()> {
    let sealed = seal_bytes(b"through the public API", 1000)?;
    let beacon = Beacon::parse(1000, SIGNATURE)?;
    assert_eq!(open_bytes(sealed.as_bytes(), &beacon)?, b"through the public API");
    Ok(())
}
