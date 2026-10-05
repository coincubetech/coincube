//! #568 B5c-1: the Split copy sweep. Every Split screen speaks Split: no
//! Claim wording reaches it through the shared coordinator copy, the record
//! is named by its descriptor digest (never a "fingerprint"), and the single
//! step names neither step 1 nor step 2.
use super::*;
use crate::{
    app::state::vault::split::unified::{
        describe_unified, describe_unified_check, describe_unified_target,
    },
    services::{claim_preflight, claim_workflow},
};

/// Whether `text` uses "claim" as a word of its own (any case). "claims"
/// and "claimed" (a coin this split claims) are Split's own words.
fn names_claim(text: &str) -> bool {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|word| word.eq_ignore_ascii_case("claim"))
}

/// One of every coordinator refusal (each `NotReady` assessment that has
/// Claim copy behind it, and each journal error).
fn every_error() -> Vec<CoordinatorError> {
    use CoordinatorError as E;
    let mut errors = vec![
        E::Unsupported,
        E::InvalidBinding,
        E::UnsafeLegacyAlternative,
        E::Revoked,
        E::InvalidReview,
        E::ChangedReview,
        E::Observation(crate::services::claim_observation::Failure {
            stage: crate::services::claim_observation::Stage::ForkTransaction,
            kind: FailureKind::Http(503),
        }),
        E::Preflight(claim_preflight::Error::BackendChanged),
        E::Preflight(claim_preflight::Error::Transport),
        E::Preflight(claim_preflight::Error::BothRoutesRejected {
            local: "bad-txns".into(),
            connect: "bad-txns".into(),
        }),
        E::PolicyRejected(claim_preflight::NodePolicy::Rejected {
            reason: "min relay fee not met".into(),
        }),
        E::PolicyRejected(claim_preflight::NodePolicy::Accepted),
        E::SubmissionAlreadyRecorded,
        E::ExpiredEvidence,
        E::CompletionPersistence("Claim Cube is missing or ambiguous".into()),
    ];
    for assessment in [
        Assessment::Reorged,
        Assessment::ExpiryMargin,
        Assessment::RdtsExpired,
        Assessment::WaitingForConfirmation,
        Assessment::WaitingForDepth { confirmations: 3 },
        Assessment::StaleObservation,
    ] {
        errors.push(E::NotReady(assessment));
    }
    for error in [
        claim_workflow::Error::Io(std::io::Error::other("disk")),
        claim_workflow::Error::Busy,
        claim_workflow::Error::Conflict,
        claim_workflow::Error::InvalidJournal,
        claim_workflow::Error::InvalidPlan,
        claim_workflow::Error::WrongIdentity,
        claim_workflow::Error::Revoked,
        claim_workflow::Error::LateObservation,
        claim_workflow::Error::Unchecked,
        claim_workflow::Error::UnsupportedPlatform,
    ] {
        errors.push(E::Journal(error));
    }
    errors
}

/// #656 F1 and the go-live sweep: no coordinator refusal reaches a Split
/// screen in Claim's words, through step 1's copy, step 2's, the
/// completion's or the single step's. Before this slice the journal,
/// backend-change, submission-recorded, completion-persistence and
/// legacy-signature refusals read "claim", "Claim" or "Claim Cube".
#[test]
fn split_copy_never_names_a_claim() {
    let n = every_error().len();
    type Describe = fn(CoordinatorError) -> String;
    let describers: [(&str, Describe); 4] = [
        ("step1::describe", step1::describe),
        ("describe_check", |e| describe_check(e).reason),
        ("describe_completion", |e| describe_completion(e).reason),
        ("describe_unified_check", |e| {
            describe_unified_check(e).reason
        }),
    ];
    for (name, describe) in describers {
        for (i, error) in every_error().into_iter().enumerate() {
            let debug = format!("{error:?}");
            let copy = describe(error);
            assert!(!copy.is_empty(), "{} {}/{}: {}", name, i, n, debug);
            assert!(!names_claim(&copy), "{} {}: {}", name, debug, copy);
        }
    }
    // The settings layer's own message never reaches the screen.
    let persisted = describe_check(CoordinatorError::CompletionPersistence(
        "Claim Cube is gone".into(),
    ));
    assert!(
        !persisted.reason.contains("Claim Cube"),
        "{}",
        persisted.reason
    );
}

/// #656 F1: a completion check, D17 recheck or deletion refused for
/// expired evidence or the completion record reads as the completion's own
/// line, retryable (#645 P3-1); the rest reads as `describe_check`.
#[test]
fn completion_refusals_have_split_completion_copy() {
    let expired = describe_completion(CoordinatorError::ExpiredEvidence);
    assert_eq!(expired.reason, COMPLETION_CHECK_EXPIRED);
    assert!(expired.retry);
    let record = describe_completion(CoordinatorError::CompletionPersistence(
        "Claim Cube is missing or ambiguous".into(),
    ));
    assert_eq!(record.reason, COMPLETION_RECORD_UNAVAILABLE);
    assert!(record.retry);
    for (error, twin) in every_error().into_iter().zip(every_error()) {
        if matches!(
            error,
            CoordinatorError::ExpiredEvidence | CoordinatorError::CompletionPersistence(_)
        ) {
            continue;
        }
        let debug = format!("{error:?}");
        assert_eq!(
            describe_completion(error),
            describe_check(twin),
            "{}",
            debug
        );
    }
}

/// #656 F2: an expiry while the completion was being saved may follow the
/// record's write, so its copy never says nothing was recorded; an expiry
/// of the check itself does. Neither deleted anything.
#[test]
fn completion_expiry_copy_says_what_may_be_recorded() {
    assert!(COMPLETION_EXPIRED.contains("may already be recorded"));
    assert!(!COMPLETION_EXPIRED.contains("nothing was recorded"));
    assert!(COMPLETION_EXPIRED.contains("Nothing was deleted"));
    assert!(COMPLETION_CHECK_EXPIRED.contains("nothing was recorded or deleted"));
}

/// #656 F4 (D9): the record holds a digest of the source descriptor, which
/// the copy names as such; a "fingerprint" is BIP32's four bytes.
#[test]
fn completion_copy_names_the_descriptor_digest_not_a_fingerprint() {
    assert!(!SPLIT_COMPLETED.contains("fingerprint"));
    assert!(SPLIT_COMPLETED.contains("digest (a hash) of the source wallet's descriptor"));
    let view = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app/view/vault/split.rs"),
    )
    .unwrap();
    assert!(
        !view.contains("fingerprint"),
        "the Split view names a fingerprint"
    );
}

/// #658: the terminal conflict's warning no longer says to check again
/// later beside its close; it names the close. #658 P3-4 (S4b-D1): the
/// close refused because the coin is unspent again names the way out (step
/// 1's saved signed transaction broadcast again elsewhere, or waiting) and
/// that the wallet never sends step 1 again (D13 = A).
#[test]
fn terminal_conflict_copy_names_its_way_out() {
    let outpoint = OutPoint::new(Txid::from_byte_array([9; 32]), 1);
    let unspent = conflict_coin_unspent_copy(&outpoint);
    assert!(unspent.contains(&outpoint.to_string()));
    assert!(unspent.contains("any Bitcoin node or service"));
    assert!(unspent.contains("or wait"));
    assert!(unspent.contains("never sends step 1 again"));
}

/// #660: the single step has no step 1 or step 2, so neither its target
/// copy nor its coordinator copy names one. The two-step copy still does.
#[test]
fn unified_copy_names_no_step() {
    let steps = |text: &str| {
        let lower = text.to_lowercase();
        lower.contains("step 1") || lower.contains("step 2") || lower.contains("step-2")
    };
    let targets = || {
        vec![
            TargetError::AlreadyReserved,
            TargetError::NotTracking,
            TargetError::NoReservation,
            TargetError::ReservationUnavailable,
            TargetError::NotTargetVault,
            TargetError::Used(ChainId::Bitcoin),
            TargetError::Used(ChainId::BitcoinBlake2b),
            TargetError::Unavailable(ChainId::BitcoinBlake2b, FailureKind::Http(503)),
        ]
    };
    for error in targets() {
        let debug = format!("{error:?}");
        let copy = describe_unified_target(error);
        assert!(!steps(&copy.reason), "{}: {}", debug, copy.reason);
    }
    // The single step's own refusals route their target errors here.
    for error in targets() {
        let debug = format!("{error:?}");
        let copy = describe_unified(
            crate::services::claim_coordinator::fork::split::step2::UnifiedError::Target(error),
        );
        assert!(!steps(&copy.reason), "{}: {}", debug, copy.reason);
    }
    for error in every_error() {
        let debug = format!("{error:?}");
        let copy = describe_unified_check(error);
        assert!(!steps(&copy.reason), "{}: {}", debug, copy.reason);
    }
    // Retry and recovery are the two-step route's.
    for (error, twin) in every_error().into_iter().zip(every_error()) {
        let debug = format!("{error:?}");
        let (unified, two_step) = (describe_unified_check(error), describe_check(twin));
        assert_eq!(
            (unified.retry, unified.recovery),
            (two_step.retry, two_step.recovery),
            "{}",
            debug
        );
    }
    assert!(steps(
        &describe_target(TargetError::Used(ChainId::Bitcoin)).reason
    ));
}
