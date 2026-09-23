//! Pin the hand-written C header to the Rust ABI.
//!
//! A hand-written header is smaller and far easier to review than a generated
//! one, which is why this crate has one. Its single real risk is drift: a
//! constant renumbered in Rust and not in the header would hand Keychain a
//! wrong code with no build error anywhere. So the header is read at compile
//! time and checked three ways — every `CC_*` value agrees, neither side has a
//! constant the other lacks, and every declared function exists as an exported
//! symbol.
//!
//! This is what replaces `cbindgen` here: the same guarantee, no build-time
//! dependency, and nothing generated to review.

const HEADER: &str = include_str!("../include/coincube_keychain_ffi.h");
const SOURCE: &str = include_str!("../src/lib.rs");

/// Every `#define CC_<NAME> <integer>` in the header, in file order.
fn header_constants() -> Vec<(String, i64)> {
    let mut found = Vec::new();
    for line in HEADER.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("#define ") else {
            continue;
        };
        let mut parts = rest.split_whitespace();
        let Some(name) = parts.next() else { continue };
        if !name.starts_with("CC_") {
            continue;
        }
        let Some(value) = parts.next() else { continue };
        let Ok(value) = value.parse::<i64>() else {
            continue;
        };
        found.push((name.to_string(), value));
    }
    found
}

/// Every `pub const CC_<NAME>` declared in `src/lib.rs`.
fn source_constants() -> Vec<String> {
    SOURCE
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = line.strip_prefix("pub const CC_")?;
            let name = rest.split(':').next()?.trim();
            Some(format!("CC_{name}"))
        })
        .collect()
}

/// Every `#[no_mangle] pub ... extern "C" fn` name in `src/lib.rs`.
fn exported_functions() -> Vec<String> {
    SOURCE
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = line
                .strip_prefix("pub extern \"C\" fn ")
                .or_else(|| line.strip_prefix("pub unsafe extern \"C\" fn "))?;
            Some(rest.split('(').next()?.trim().to_string())
        })
        .collect()
}

/// Every `coincube_*` function the header declares.
fn header_functions() -> Vec<String> {
    let mut found = Vec::new();
    for (index, _) in HEADER.match_indices("coincube_") {
        let tail = &HEADER[index..];
        let name: String = tail
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        // Only count a declaration: the name is immediately followed by `(`.
        if tail[name.len()..].starts_with('(') && !found.contains(&name) {
            found.push(name);
        }
    }
    found
}

/// The authoritative value of each exported constant, read from Rust.
///
/// Written out rather than parsed so that a renumbering has to be made here
/// too, deliberately, in front of a reviewer.
fn rust_values() -> Vec<(&'static str, i64)> {
    use coincube_keychain_ffi::*;
    vec![
        ("CC_DIGEST_LEN", CC_DIGEST_LEN as i64),
        ("CC_OK", CC_OK as i64),
        ("CC_ERR_NULL_ARGUMENT", CC_ERR_NULL_ARGUMENT as i64),
        ("CC_ERR_BUFFER_TOO_SMALL", CC_ERR_BUFFER_TOO_SMALL as i64),
        (
            "CC_ERR_INVALID_TRANSACTION",
            CC_ERR_INVALID_TRANSACTION as i64,
        ),
        (
            "CC_ERR_INVALID_SPENT_OUTPUTS",
            CC_ERR_INVALID_SPENT_OUTPUTS as i64,
        ),
        ("CC_ERR_INVALID_UTF8", CC_ERR_INVALID_UTF8 as i64),
        ("CC_ERR_UNKNOWN_NETWORK", CC_ERR_UNKNOWN_NETWORK as i64),
        ("CC_ERR_PANIC", CC_ERR_PANIC as i64),
        (
            "CC_ERR_MISSING_UNIFIED_FLAG",
            CC_ERR_MISSING_UNIFIED_FLAG as i64,
        ),
        (
            "CC_ERR_UNSUPPORTED_SCRIPT_TYPE",
            CC_ERR_UNSUPPORTED_SCRIPT_TYPE as i64,
        ),
        (
            "CC_ERR_PREVOUTS_LENGTH_MISMATCH",
            CC_ERR_PREVOUTS_LENGTH_MISMATCH as i64,
        ),
        (
            "CC_ERR_INPUT_INDEX_OUT_OF_BOUNDS",
            CC_ERR_INPUT_INDEX_OUT_OF_BOUNDS as i64,
        ),
        (
            "CC_ERR_INPUT_INDEX_TOO_LARGE",
            CC_ERR_INPUT_INDEX_TOO_LARGE as i64,
        ),
        (
            "CC_ERR_MISSING_SINGLE_OUTPUT",
            CC_ERR_MISSING_SINGLE_OUTPUT as i64,
        ),
        ("CC_ERR_INVALID_PSBT", CC_ERR_INVALID_PSBT as i64),
        ("CC_ERR_PSBT_VALIDATION", CC_ERR_PSBT_VALIDATION as i64),
        ("CC_ERR_INVALID_MNEMONIC", CC_ERR_INVALID_MNEMONIC as i64),
        ("CC_ERR_SIGNING", CC_ERR_SIGNING as i64),
        ("CC_ERR_EXPORT", CC_ERR_EXPORT as i64),
        ("CC_NETWORK_BITCOIN", CC_NETWORK_BITCOIN as i64),
        ("CC_NETWORK_TESTNET", CC_NETWORK_TESTNET as i64),
        ("CC_NETWORK_SIGNET", CC_NETWORK_SIGNET as i64),
        ("CC_NETWORK_REGTEST", CC_NETWORK_REGTEST as i64),
    ]
}

#[test]
fn header_constant_values_match_rust() {
    let rust = rust_values();
    for (name, header_value) in header_constants() {
        let (_, rust_value) = rust
            .iter()
            .find(|(candidate, _)| *candidate == name)
            .unwrap_or_else(|| {
                panic!("header defines {name}, which this test does not know about")
            });
        assert_eq!(
            *rust_value, header_value,
            "{name} is {rust_value} in Rust and {header_value} in the header"
        );
    }
}

#[test]
fn neither_side_has_a_constant_the_other_lacks() {
    let mut in_header: Vec<String> = header_constants()
        .into_iter()
        .map(|(name, _)| name)
        // CC_DIGEST_LEN is a length, not a status code, and is declared in Rust
        // as a `pub const` too, so it belongs in this comparison.
        .collect();
    let mut in_source = source_constants();
    in_header.sort();
    in_header.dedup();
    in_source.sort();
    in_source.dedup();
    assert_eq!(
        in_source, in_header,
        "the header and src/lib.rs disagree about which CC_* constants exist"
    );
}

#[test]
fn every_declared_function_is_exported() {
    let mut declared = header_functions();
    let mut exported = exported_functions();
    declared.sort();
    exported.sort();
    assert_eq!(
        exported, declared,
        "the header and src/lib.rs disagree about the exported functions"
    );
    assert!(
        !declared.is_empty(),
        "parsing found no functions at all, which means this test is not \
         checking anything"
    );
}
