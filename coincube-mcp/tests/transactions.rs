#![cfg(any(unix, windows))]
use coincube_mcp::{ipc::Grant, transaction::*, *};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

fn input() -> PrepareInput {
    PrepareInput {
        wallet_id: "wallet".into(),
        request_id: "payment-1".into(),
        recipients: vec![Recipient {
            address: "bcrt1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqdku202".into(),
            amount_sats: 10_000,
        }],
        fee_rate_sat_vb: 2,
        max_fee_sats: 1_000,
    }
}
#[test]
fn transaction_contract_rejects_ambiguous_units_unbounded_values_and_extra_authority() {
    let base = input();
    assert!(base.valid());
    let json = serde_json::to_value(&base).unwrap();
    for (name, value) in [
        ("psbt", serde_json::json!("external")),
        ("broadcast", serde_json::json!(true)),
        ("change_address", serde_json::json!("external")),
        ("network", serde_json::json!("bitcoin")),
        ("fee_rate_sat_vb", serde_json::json!("2")),
        ("max_fee_sats", serde_json::json!(-1)),
    ] {
        let mut invalid = json.clone();
        invalid[name] = value;
        assert!(serde_json::from_value::<PrepareInput>(invalid).is_err());
    }
    let mut cases = Vec::new();
    for id in ["", "../wallet", "file:///tmp/wallet", "bad\nselector"] {
        let mut i = base.clone();
        i.wallet_id = id.into();
        cases.push(i);
    }
    for rate in [0, 1001, u64::MAX] {
        let mut i = base.clone();
        i.fee_rate_sat_vb = rate;
        cases.push(i);
    }
    for amount in [0, MAX_SATS, u64::MAX] {
        let mut i = base.clone();
        i.recipients[0].amount_sats = amount;
        cases.push(i);
    }
    let mut empty = base.clone();
    empty.recipients.clear();
    cases.push(empty);
    let mut many = base.clone();
    many.recipients = vec![many.recipients[0].clone(); MAX_RECIPIENTS + 1];
    cases.push(many);
    let mut uri = base.clone();
    uri.recipients[0].address = "bitcoin:address?amount=0.1".into();
    cases.push(uri);
    for i in cases {
        assert!(!i.valid(), "invalid contract was accepted");
    }
}

#[test]
fn transaction_authority_cannot_be_obtained_from_old_scopes_or_used_to_read_sign_or_admit_creation()
{
    let credential = "a".repeat(64);
    let tx = Grant::paired_transactions("wallet".into(), credential.clone(), || true).unwrap();
    let prepare = Request::PrepareTransaction(input());
    let get = Request::Transaction(DraftInput {
        wallet_id: "wallet".into(),
        draft_id: "draft".into(),
    });
    assert!(tx.authorize(&credential, &prepare).is_ok());
    assert!(tx.authorize(&credential, &get).is_ok());
    assert!(tx.authorize(&credential, &Request::List(Empty {})).is_ok());
    for request in [
        Request::Summary(WalletInput {
            wallet_id: "wallet".into(),
        }),
        Request::History(HistoryInput {
            wallet_id: "wallet".into(),
            limit: 1,
        }),
        Request::Policy(WalletInput {
            wallet_id: "wallet".into(),
        }),
        Request::Proposal(proposal::ProposalInput {
            proposal_id: "draft".into(),
        }),
    ] {
        assert_eq!(
            tx.authorize(&credential, &request),
            Err(Error::AccessDenied)
        );
    }
    let mut foreign = input();
    foreign.wallet_id = "other".into();
    assert_eq!(
        tx.authorize(&credential, &Request::PrepareTransaction(foreign)),
        Err(Error::AccessDenied)
    );
    assert_eq!(tx.admit_native(|| Ok(())), Err(Error::AccessDenied));
    for old in [
        Grant::new("wallet".into(), [true; 3], Duration::from_secs(60)),
        Grant::paired_proposals(credential.clone(), || true).unwrap(),
        Grant::paired_creation(credential.clone(), || true).unwrap(),
    ] {
        assert_eq!(
            old.authorize(old.credential(), &prepare),
            Err(Error::AccessDenied)
        );
        assert_eq!(
            old.authorize(old.credential(), &get),
            Err(Error::AccessDenied)
        );
    }
    let active = Arc::new(AtomicBool::new(true));
    let guard = active.clone();
    let restored = Grant::paired_transactions("wallet".into(), credential.clone(), move || {
        guard.load(Ordering::SeqCst)
    })
    .unwrap();
    active.store(false, Ordering::SeqCst);
    assert_eq!(
        restored.authorize(&credential, &prepare),
        Err(Error::AccessDenied)
    );
    tx.revoke();
    assert_eq!(
        tx.authorize(&credential, &prepare),
        Err(Error::AccessDenied)
    );
}
