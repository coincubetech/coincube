//! Signed macOS probe for the production imported-inventory Keychain adapter.
//!
//! Each phase is a separate process and uses a dedicated synthetic service.
//! The normal unit tests exercise the anchor codec with an in-memory store.

#[cfg(target_os = "macos")]
#[path = "../src/services/imported_inventory_anchor/apple.rs"]
mod apple;

#[cfg(target_os = "macos")]
const SERVICE: &str = "io.coincube.tenshu.native-imported-anchor-probe.v1";

#[cfg(target_os = "macos")]
fn main() {
    use uuid::Uuid;

    let mut args = std::env::args().skip(1);
    let phase = args.next().expect("phase is required");
    let account = Uuid::parse_str(&args.next().expect("run UUID is required"))
        .unwrap()
        .to_string();
    assert!(args.next().is_none(), "unexpected argument");
    let old = b"synthetic committed generation 0".as_slice();
    let pending = b"synthetic pending generation 1".as_slice();
    let committed = b"synthetic committed generation 1 cleanup".as_slice();
    let clear = b"synthetic committed generation 1 clear".as_slice();
    match phase.as_str() {
        "create" => {
            assert_eq!(apple::load(SERVICE, &account).unwrap(), None);
            apple::create(SERVICE, &account, old).unwrap();
            assert_eq!(
                apple::load(SERVICE, &account).unwrap().as_deref(),
                Some(old)
            );
        }
        "pending" => {
            assert_eq!(
                apple::load(SERVICE, &account).unwrap().as_deref(),
                Some(old)
            );
            apple::update(SERVICE, &account, pending).unwrap();
            assert_eq!(
                apple::load(SERVICE, &account).unwrap().as_deref(),
                Some(pending)
            );
        }
        "commit" => {
            assert_eq!(
                apple::load(SERVICE, &account).unwrap().as_deref(),
                Some(pending)
            );
            apple::update(SERVICE, &account, committed).unwrap();
            assert_eq!(
                apple::load(SERVICE, &account).unwrap().as_deref(),
                Some(committed)
            );
        }
        "clear" => {
            assert_eq!(
                apple::load(SERVICE, &account).unwrap().as_deref(),
                Some(committed)
            );
            apple::update(SERVICE, &account, clear).unwrap();
            assert_eq!(
                apple::load(SERVICE, &account).unwrap().as_deref(),
                Some(clear)
            );
        }
        "verify" => {
            assert_eq!(
                apple::load(SERVICE, &account).unwrap().as_deref(),
                Some(clear)
            );
        }
        "delete" => delete(&account),
        _ => panic!("unknown phase"),
    }
    println!("imported anchor native phase {phase}: ok");
}

#[cfg(target_os = "macos")]
fn delete(account: &str) {
    use core_foundation::{base::TCFType, dictionary::CFDictionary, string::CFString};
    use security_framework_sys::{
        base::errSecItemNotFound,
        item::{
            kSecAttrAccessGroup, kSecAttrAccount, kSecAttrService, kSecClass,
            kSecClassGenericPassword,
        },
        keychain_item::SecItemDelete,
    };
    extern "C" {
        static kSecUseDataProtectionKeychain: core_foundation::string::CFStringRef;
    }
    let key = |value| unsafe { CFString::wrap_under_get_rule(value) };
    let query = CFDictionary::from_CFType_pairs(&[
        (
            key(unsafe { kSecUseDataProtectionKeychain }),
            core_foundation::boolean::CFBoolean::from(true).into_CFType(),
        ),
        (
            key(unsafe { kSecClass }),
            key(unsafe { kSecClassGenericPassword }).into_CFType(),
        ),
        (
            key(unsafe { kSecAttrService }),
            CFString::new(SERVICE).into_CFType(),
        ),
        (
            key(unsafe { kSecAttrAccount }),
            CFString::new(account).into_CFType(),
        ),
        (
            key(unsafe { kSecAttrAccessGroup }),
            CFString::new("8UVR249AD5.io.coincube.tenshu").into_CFType(),
        ),
    ]);
    let status = unsafe { SecItemDelete(query.as_concrete_TypeRef()) };
    assert!(
        status == 0 || status == errSecItemNotFound,
        "Keychain deletion failed: {status}"
    );
    assert_eq!(apple::load(SERVICE, account).unwrap(), None);
}

#[cfg(not(target_os = "macos"))]
fn main() {
    panic!("the native imported-anchor probe requires macOS");
}
