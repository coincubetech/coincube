//! What a failed Connect call says about this computer and its network.
//!
//! The realtime stream and the App act on these instead of on error strings:
//! a device Connect no longer knows is renewed automatically, a removed one
//! is never renewed, and a dead network reads as Offline rather than as
//! Connect being down.
//!
//! Server contract (coincube-api, "S1"): a missing or wrong-owner device row
//! answers `NOT_FOUND` with reason `DEVICE_UNKNOWN`; a revoked row answers
//! `PERMISSION_DENIED` with reason `DEVICE_REVOKED`; a lookup that didn't
//! complete answers `UNAVAILABLE`. The reason travels in the
//! [`REASON_METADATA`] trailer or as the first word of the status message.
//! Servers from before S1 answer a bare `PERMISSION_DENIED` for all three,
//! which is [`Refusal::Unexplained`] here and is never renewed on its own:
//! renewing it could undo a removal.

/// The trailer that carries the machine-readable reason for a refusal.
pub const REASON_METADATA: &str = "x-connect-reason";

/// Connect refused this computer's device id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Connect has no record of this id for the account. Safe to renew.
    DeviceUnknown,
    /// The computer was removed from the account. Never renewed; the user
    /// signs in again.
    DeviceRevoked,
    /// A bare `PERMISSION_DENIED` from a server without reasons. It may be
    /// any of the above or a failed lookup, so Tenshu re-checks slowly and
    /// leaves renewal to the user (Try again).
    Unexplained,
}

/// Why live updates aren't running, when Connect didn't refuse the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trouble {
    /// No usable network: the Connect host couldn't be resolved or the
    /// network is unreachable.
    Offline,
    /// The network works but Connect doesn't answer, or the stream dropped.
    Unreachable,
}

fn reason(status: &tonic::Status) -> Option<String> {
    if let Some(value) = status.metadata().get(REASON_METADATA) {
        if let Ok(value) = value.to_str() {
            return Some(value.trim().to_ascii_uppercase());
        }
    }
    let first = status
        .message()
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .next()?;
    first
        .starts_with("DEVICE_")
        .then(|| first.to_ascii_uppercase())
}

/// The refusal a status carries, if it is one.
pub fn refusal(status: &tonic::Status) -> Option<Refusal> {
    let reason = reason(status);
    match status.code() {
        tonic::Code::NotFound => Some(Refusal::DeviceUnknown),
        tonic::Code::PermissionDenied => Some(match reason.as_deref() {
            Some("DEVICE_UNKNOWN") => Refusal::DeviceUnknown,
            Some("DEVICE_REVOKED") => Refusal::DeviceRevoked,
            _ => Refusal::Unexplained,
        }),
        _ => None,
    }
}

/// Phrases the resolver and the OS use when there is no network. Matched on
/// the whole error chain, lower-cased.
const OFFLINE_SIGNS: &[&str] = &[
    "dns error",
    "failed to lookup address",
    "nodename nor servname",
    "name or service not known",
    "temporary failure in name resolution",
    "no such host is known",
    "network is unreachable",
    "network unreachable",
    "no route to host",
];

/// Offline when anything in the error chain says there is no network;
/// otherwise Connect is unreachable.
pub fn trouble(error: &(dyn std::error::Error + 'static)) -> Trouble {
    let mut current = Some(error);
    while let Some(e) = current {
        if let Some(io) = e.downcast_ref::<std::io::Error>() {
            if matches!(
                io.kind(),
                std::io::ErrorKind::NetworkUnreachable | std::io::ErrorKind::HostUnreachable
            ) {
                return Trouble::Offline;
            }
        }
        if offline_text(&e.to_string()) {
            return Trouble::Offline;
        }
        current = e.source();
    }
    Trouble::Unreachable
}

fn offline_text(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    OFFLINE_SIGNS.iter().any(|sign| text.contains(sign))
}

/// [`trouble`] for a status: its message first (tonic folds transport
/// failures into it), then its source chain.
pub fn status_trouble(status: &tonic::Status) -> Trouble {
    if offline_text(status.message()) {
        return Trouble::Offline;
    }
    match std::error::Error::source(status) {
        Some(source) => trouble(source),
        None => Trouble::Unreachable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_found_is_an_unknown_device() {
        let status = tonic::Status::not_found("DEVICE_UNKNOWN: no such device");
        assert_eq!(refusal(&status), Some(Refusal::DeviceUnknown));
        assert_eq!(
            refusal(&tonic::Status::not_found("device not found")),
            Some(Refusal::DeviceUnknown)
        );
    }

    #[test]
    fn revoked_needs_the_reason() {
        assert_eq!(
            refusal(&tonic::Status::permission_denied(
                "DEVICE_REVOKED: device was removed"
            )),
            Some(Refusal::DeviceRevoked)
        );
        let mut status = tonic::Status::permission_denied("device revoked");
        status
            .metadata_mut()
            .insert(REASON_METADATA, "device_revoked".parse().unwrap());
        assert_eq!(refusal(&status), Some(Refusal::DeviceRevoked));
    }

    #[test]
    fn a_bare_denial_from_an_old_server_is_never_renewable() {
        for message in [
            "device not found or not owned by user",
            "device revoked",
            "",
        ] {
            assert_eq!(
                refusal(&tonic::Status::permission_denied(message)),
                Some(Refusal::Unexplained),
                "{:?}",
                message
            );
        }
    }

    #[test]
    fn other_codes_are_not_refusals() {
        for status in [
            tonic::Status::unavailable("upstream down"),
            tonic::Status::unauthenticated("token expired"),
            tonic::Status::aborted("connection superseded"),
            tonic::Status::internal("failed to register connection"),
            tonic::Status::invalid_argument("invalid device_id"),
        ] {
            assert_eq!(refusal(&status), None, "{:?}", status.code());
        }
    }

    #[test]
    fn dns_and_unreachable_networks_read_as_offline() {
        let dns = std::io::Error::other("dns error: failed to lookup address information");
        assert_eq!(trouble(&dns), Trouble::Offline);
        let net = std::io::Error::from(std::io::ErrorKind::NetworkUnreachable);
        assert_eq!(trouble(&net), Trouble::Offline);
        assert_eq!(
            status_trouble(&tonic::Status::unavailable(
                "error trying to connect: dns error: nodename nor servname provided"
            )),
            Trouble::Offline
        );
    }

    #[test]
    fn refused_connections_and_timeouts_read_as_unreachable() {
        let refused = std::io::Error::from(std::io::ErrorKind::ConnectionRefused);
        assert_eq!(trouble(&refused), Trouble::Unreachable);
        assert_eq!(
            status_trouble(&tonic::Status::deadline_exceeded(
                "Connect opening handshake timed out"
            )),
            Trouble::Unreachable
        );
        assert_eq!(
            status_trouble(&tonic::Status::unavailable("upstream down")),
            Trouble::Unreachable
        );
    }
}
