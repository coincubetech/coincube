//! Split (#568 S4) reorg recovery after the step-2 submission, D13 = A: step
//! 1 is never resent. Step 2 is submitted through the coordinator over the
//! Connect route, then the journal is reopened as a restart does; the
//! synthetic view says where step 1 is on Bitcoin.
use super::*;
use crate::services::claim_coordinator::fork::split::step2::SplitStep2Reconciler;

/// Step 2 reviewed and submitted through the coordinator, which is then
/// dropped: a journal with a recorded step-2 submission, and its txid.
async fn submitted() -> (Harness, Txid) {
    let s = Step2::new().await;
    let signed_tx = s.signed_tx();
    let (transport, _server, _, _) = transport(&s.h, &signed_tx, true).await;
    let (h, mut coordinator) = finish(s, transport);
    let review = coordinator.prepare_review(&context()).await.unwrap();
    coordinator
        .confirm_and_submit(review, &context())
        .await
        .unwrap();
    drop(coordinator);
    (h, signed_tx.compute_txid())
}
/// The journal reopened by the reconciler over `services`.
fn reopen(h: &Harness, services: Box<dyn SplitForkServices>) -> SplitStep2Reconciler {
    SplitStep2Reconciler::open(
        &h.temp.0,
        TARGET.into(),
        h.step1.source().digest(),
        context(),
        h.sender.subscribe(),
        services,
        policy(),
    )
    .unwrap()
}
/// The step-1 coordinator's own open on `h`'s journal, as a restart's
/// `resume_split` reaches it.
fn resume_step1(h: &Harness) -> Result<Step1Coordinator, Error> {
    Step1Coordinator::open_split(
        &h.temp.0,
        TARGET.into(),
        &h.step1,
        h.verified(),
        FORK,
        context(),
        h.sender.subscribe(),
        Box::new(h.chains.clone()),
        policy(),
        true,
    )
}
/// `h`'s journal reopened and verified as the step-1 coordinator's own
/// controller, without the coordinator's open.
fn step1_controller(h: &Harness) -> Controller {
    let identity = claim_workflow::split_identity(TARGET.into(), h.step1.source().digest());
    let mut controller =
        Controller::reopen_settling_blocking(&h.temp.0, &identity, context()).unwrap();
    controller
        .revalidate_split_construction(&context(), &h.step1, FORK)
        .unwrap();
    controller
        .bind_recovered_split_transaction(&context(), &h.verified())
        .unwrap();
    controller
}
/// Step 1 reorged out of Bitcoin, with a tip the step-1 preflight mock
/// answers for: a resend of step 1 would be reviewable.
fn step1_gone(h: &Harness) {
    h.chains.edit(|view| {
        view.step1_block = None;
        view.bitcoin_tip = BlockRef {
            height: 106,
            hash: hash(0x40),
        };
    });
}

/// S4 guard: once a step-2 submission is recorded, the step-1 coordinator
/// never reopens the journal, so step 1 can't be reviewed, resent or
/// reconfirmed there again. Before it, the same open succeeds.
#[tokio::test(flavor = "multi_thread")]
async fn split_resume_refuses_a_recorded_step2_submission() {
    let before = Harness::new(6).await;
    assert!(resume_step1(&before).is_ok());

    let (h, _) = submitted().await;
    assert!(matches!(
        resume_step1(&h),
        Err(Error::SubmissionAlreadyRecorded)
    ));
    // Nothing was written by the refused open, and the reconciler still
    // opens it.
    let journal = h.temp.journal();
    assert!(journal["fork_submission"].is_object());
    drop(reopen(&h, Box::new(h.chains.clone())));
    assert_eq!(h.temp.journal(), journal);
}

/// S4 guard: the journal itself refuses a step-1 resend once a step-2
/// submission is recorded, whatever coordinator holds it. A step-1
/// coordinator opened before the record (its controller replaced by one
/// reopened on a journal with step 2 recorded) can neither prepare a resend
/// nor confirm one it reviewed before; nothing is recorded or sent.
#[tokio::test(flavor = "multi_thread")]
async fn split_resubmission_is_refused_after_a_step2_submission() {
    // A journal with step 2 recorded, and step 1 reorged out of Bitcoin.
    let (recorded, _) = submitted().await;
    step1_gone(&recorded);
    let before = recorded.temp.journal();

    // Prepare: a coordinator opened on a journal without step 2, then
    // holding the recorded one.
    let fresh = Harness::new(6).await;
    step1_gone(&fresh);
    let mut coordinator = resume_step1(&fresh).unwrap();
    drop(std::mem::replace(
        &mut coordinator.controller,
        step1_controller(&recorded),
    ));
    assert!(matches!(
        coordinator.prepare_resubmission(&context()).await,
        Err(Error::NotReady(_))
    ));
    drop(coordinator);

    // Confirm: the resend was reviewed while the journal had no step 2.
    let mut coordinator = resume_step1(&fresh).unwrap();
    let review = coordinator.prepare_resubmission(&context()).await.unwrap();
    assert_eq!(review.snapshot().transaction, fresh.signed);
    drop(std::mem::replace(
        &mut coordinator.controller,
        step1_controller(&recorded),
    ));
    // The step-1 services panic on any submission.
    assert!(matches!(
        coordinator.confirm_resubmission(review, &context()).await,
        Err(Error::NotReady(_))
    ));
    drop(coordinator);
    let after = recorded.temp.journal();
    assert_eq!(after["bitcoin_attempts"], before["bitcoin_attempts"]);
    assert_eq!(after, before);
}
