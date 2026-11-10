//! The published C header declares exactly the functions and types of `src/abi`.

use std::{env, fs, path::Path};

const HEADER: &str = "include/scalardb_analytics_connector.h";

/// Set to regenerate the committed header from the Rust declarations.
const UPDATE: &str = "SCALARDB_ANALYTICS_CONNECTOR_UPDATE_HEADER";

#[test]
fn committed_header_matches_the_rust_declarations() {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let config = cbindgen::Config::from_file(crate_dir.join("cbindgen.toml")).unwrap();
    let mut generated = Vec::new();
    cbindgen::Builder::new()
        .with_crate(crate_dir)
        .with_config(config)
        .generate()
        .unwrap()
        .write(&mut generated);
    let generated = String::from_utf8(generated).unwrap();
    let header = crate_dir.join(HEADER);
    if env::var_os(UPDATE).is_some() {
        fs::write(&header, &generated).unwrap();
    }
    let committed = fs::read_to_string(&header).unwrap_or_default();
    assert!(
        committed == generated,
        "{HEADER} differs from the Rust declarations; run the test with {UPDATE}=1 to regenerate it"
    );
}
