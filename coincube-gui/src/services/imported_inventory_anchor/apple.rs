//! Dedicated data-protection Keychain item for imported inventory freshness.
//!
//! Item updates are read back, but Apple does not document sudden-power-loss
//! durability of `SecItemUpdate`. The native signed-app restart and fault tests
//! are a release gate; this adapter makes no stronger durability claim.

use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::data::CFData;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::{CFString, CFStringRef};
use security_framework_sys::access_control::kSecAttrAccessibleWhenUnlockedThisDeviceOnly;
use security_framework_sys::base::{errSecDuplicateItem, errSecItemNotFound};
use security_framework_sys::item::{
    kSecAttrAccessGroup, kSecAttrAccount, kSecAttrService, kSecClass, kSecClassGenericPassword,
    kSecReturnAttributes, kSecReturnData, kSecValueData,
};
use security_framework_sys::keychain_item::{SecItemAdd, SecItemCopyMatching, SecItemUpdate};

extern "C" {
    static kSecAttrAccessible: CFStringRef;
    static kSecAttrSynchronizable: CFStringRef;
    static kSecUseDataProtectionKeychain: CFStringRef;
}

const ACCESS_GROUP: &str = "8UVR249AD5.io.coincube.tenshu";

fn cfstr(value: CFStringRef) -> CFString {
    unsafe { CFString::wrap_under_get_rule(value) }
}

fn identity(service: &str, account: &str) -> Vec<(CFString, CFType)> {
    vec![
        (
            cfstr(unsafe { kSecUseDataProtectionKeychain }),
            CFBoolean::from(true).into_CFType(),
        ),
        (
            cfstr(unsafe { kSecClass }),
            cfstr(unsafe { kSecClassGenericPassword }).into_CFType(),
        ),
        (
            cfstr(unsafe { kSecAttrService }),
            CFString::new(service).into_CFType(),
        ),
        (
            cfstr(unsafe { kSecAttrAccount }),
            CFString::new(account).into_CFType(),
        ),
        (
            cfstr(unsafe { kSecAttrAccessGroup }),
            CFString::new(ACCESS_GROUP).into_CFType(),
        ),
    ]
}

fn verify_attributes(service: &str, account: &str) -> Result<(), String> {
    let mut query = identity(service, account);
    query.push((
        cfstr(unsafe { kSecReturnAttributes }),
        CFBoolean::from(true).into_CFType(),
    ));
    let params = CFDictionary::from_CFType_pairs(&query);
    let mut output: core_foundation_sys::base::CFTypeRef = std::ptr::null();
    let status = unsafe { SecItemCopyMatching(params.as_concrete_TypeRef(), &mut output) };
    if status != 0 || output.is_null() {
        return Err("Imported inventory anchor attributes are unavailable.".into());
    }
    let attrs: CFDictionary<CFString, CFType> =
        unsafe { CFDictionary::wrap_under_create_rule(output as _) };
    let string_attr = |key| {
        attrs
            .find(cfstr(key))
            .and_then(|value| value.downcast::<CFString>())
            .map(|value| value.to_string())
    };
    if string_attr(unsafe { kSecAttrService }).as_deref() != Some(service)
        || string_attr(unsafe { kSecAttrAccount }).as_deref() != Some(account)
        || string_attr(unsafe { kSecAttrAccessGroup }).as_deref() != Some(ACCESS_GROUP)
        || string_attr(unsafe { kSecAttrAccessible }).as_deref()
            != Some(
                cfstr(unsafe { kSecAttrAccessibleWhenUnlockedThisDeviceOnly })
                    .to_string()
                    .as_str(),
            )
    {
        return Err("Imported inventory anchor identity or device-only attributes changed.".into());
    }
    let synchronizable = attrs
        .find(cfstr(unsafe { kSecAttrSynchronizable }))
        .and_then(|value| value.downcast::<CFBoolean>())
        .map(Into::<bool>::into)
        .unwrap_or(false);
    if synchronizable {
        return Err("Imported inventory anchor is synchronizable.".into());
    }
    Ok(())
}

pub(super) fn load(service: &str, account: &str) -> Result<Option<Vec<u8>>, String> {
    let mut query = identity(service, account);
    query.push((
        cfstr(unsafe { kSecReturnData }),
        CFBoolean::from(true).into_CFType(),
    ));
    let params = CFDictionary::from_CFType_pairs(&query);
    let mut output: core_foundation_sys::base::CFTypeRef = std::ptr::null();
    let status = unsafe { SecItemCopyMatching(params.as_concrete_TypeRef(), &mut output) };
    if status == errSecItemNotFound {
        return Ok(None);
    }
    if status != 0 || output.is_null() {
        return Err(format!(
            "Cannot read imported inventory anchor (OSStatus {status})."
        ));
    }
    let data = unsafe { CFData::wrap_under_create_rule(output as _) };
    verify_attributes(service, account)?;
    Ok(Some(data.bytes().to_vec()))
}

pub(super) fn create(service: &str, account: &str, value: &[u8]) -> Result<(), String> {
    let mut query = identity(service, account);
    query.extend([
        (
            cfstr(unsafe { kSecAttrAccessible }),
            cfstr(unsafe { kSecAttrAccessibleWhenUnlockedThisDeviceOnly }).into_CFType(),
        ),
        (
            cfstr(unsafe { kSecAttrSynchronizable }),
            CFBoolean::from(false).into_CFType(),
        ),
        (
            cfstr(unsafe { kSecValueData }),
            CFData::from_buffer(value).into_CFType(),
        ),
    ]);
    let params = CFDictionary::from_CFType_pairs(&query);
    let status = unsafe { SecItemAdd(params.as_concrete_TypeRef(), std::ptr::null_mut()) };
    if status == errSecDuplicateItem {
        return Err("Imported inventory anchor already exists.".into());
    }
    if status != 0 {
        return Err(format!(
            "Cannot create imported inventory anchor (OSStatus {status})."
        ));
    }
    verify_attributes(service, account)
}

pub(super) fn update(service: &str, account: &str, value: &[u8]) -> Result<(), String> {
    let query = CFDictionary::from_CFType_pairs(&identity(service, account));
    let attributes = CFDictionary::from_CFType_pairs(&[(
        cfstr(unsafe { kSecValueData }),
        CFData::from_buffer(value).into_CFType(),
    )]);
    let status = unsafe {
        SecItemUpdate(
            query.as_concrete_TypeRef(),
            attributes.as_concrete_TypeRef(),
        )
    };
    if status != 0 {
        return Err(format!(
            "Cannot update imported inventory anchor (OSStatus {status})."
        ));
    }
    verify_attributes(service, account)
}
