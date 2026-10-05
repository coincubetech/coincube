//! #568 D19 (Reviewer-664 F1): journal discovery passes over a split this
//! Cube records as completed, so one Vault can take several sources one
//! after another, and resumes every other journal: in flight, in a dead end
//! or with a step-1 conflict, unreadable or busy. A live split is never
//! hidden. The journals are real ones (`Journal`), placed under a journal
//! root as discovery finds them.
use super::*;
use crate::app::settings::SplitFromRecord;

/// What a placed journal holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Placed {
    /// Step 2 recorded and seen on BTCB2, the descriptors deleted: what a
    /// finished completion leaves.
    Completed,
    /// Step 1 recorded and tracked, no step 2 yet.
    InFlight,
    /// A recorded step 2 never seen and never returned: #625's dead end
    /// (the descriptors deleted, to show they do not decide it).
    Step2DeadEnd,
    /// [`Self::Completed`] with a terminal step-1 conflict recorded (O4).
    Conflict,
}

/// The journal's source descriptors deleted, as a finished completion
/// leaves them (D18), written as the journal stores it.
fn forget(journal: &Journal) {
    let path = journal.temp.0.join("intent.json");
    let mut intent: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    intent["split"]
        .as_object_mut()
        .unwrap()
        .remove("descriptors");
    std::fs::write(&path, serde_json::to_vec(&intent).unwrap()).unwrap();
    assert!(journal
        .lock()
        .recorded_split()
        .unwrap()
        .unwrap()
        .source
        .is_none());
}

/// A real journal of `placed`, copied to `<root>/<digest>/` with the
/// journal's own directory privacy. Returns its target Cube id, its source
/// digest, its directory and the `split_from` record a completion of it
/// writes (whether or not the caller keeps it).
pub(crate) fn place_journal(
    root: &Path,
    placed: Placed,
) -> (String, sha256::Hash, PathBuf, SplitFromRecord) {
    let journal = match placed {
        Placed::Completed | Placed::Conflict => Journal::returned(true),
        Placed::InFlight => Journal::new(false),
        Placed::Step2DeadEnd => Journal::new(true),
    };
    if placed != Placed::InFlight {
        forget(&journal);
    }
    if placed == Placed::Conflict {
        super::close::record_conflict(&journal, true);
    }
    let controller = journal.lock();
    assert_eq!(
        controller.split_step2_dead_end(),
        placed == Placed::Step2DeadEnd
    );
    let step2_txid = controller
        .recorded_split_step2()
        .map(Transaction::compute_txid)
        .unwrap_or_else(|| Txid::from_byte_array([9; 32]));
    drop(controller);
    let digest = journal.digest();
    let directory = step1::journal_directory(root, digest);
    claim_workflow::prepare_directory(&directory).unwrap();
    std::fs::copy(
        journal.temp.0.join("intent.json"),
        directory.join("intent.json"),
    )
    .unwrap();
    let record = SplitFromRecord {
        descriptor_digest: digest,
        completed_height: 1_000,
        step2_txid,
    };
    (TARGET.to_string(), digest, directory, record)
}

fn journal_root() -> (Temp, PathBuf) {
    let temp = Temp::new();
    let root = temp.0.parent().unwrap().join("split");
    claim_workflow::prepare_directory(&root).unwrap();
    (temp, root)
}

/// D19: a completed split this Cube records (digest and step-2 txid, its
/// descriptors deleted, in no dead end) is passed over; the same journal is
/// resumed when the Cube has no such record, a record of another step 2 or
/// of another source, or when the journal is another Cube's. Discovery then
/// finds the next journal under the root.
/// CF: discovery takes the first journal again (`discover`), or the check
/// ignores the step-2 txid.
#[test]
fn discovery_passes_over_a_completed_split_this_cube_records() {
    let (_temp, root) = journal_root();
    let (target, digest, directory, record) = place_journal(&root, Placed::Completed);
    assert!(step1::is_recorded_complete(
        &directory,
        &target,
        digest,
        std::slice::from_ref(&record)
    ));
    assert_eq!(
        step1::discover_resumable(&root, &target, std::slice::from_ref(&record)),
        None
    );
    // Without this Cube's record of it, or with a record of another step 2
    // or another source, it is resumed.
    let another_step2 = SplitFromRecord {
        step2_txid: Txid::from_byte_array([8; 32]),
        ..record.clone()
    };
    let another_source = SplitFromRecord {
        descriptor_digest: sha256::Hash::hash(b"another source"),
        ..record.clone()
    };
    for records in [vec![], vec![another_step2], vec![another_source]] {
        assert_eq!(
            step1::discover_resumable(&root, &target, &records),
            Some((digest, directory.clone())),
            "{:?}",
            records
        );
    }
    // Another Cube's journal (the identity names another target).
    assert_eq!(
        step1::discover_resumable(&root, "another-cube", std::slice::from_ref(&record)),
        Some((digest, directory.clone()))
    );
    // Another journal under the root is found past the completed one.
    let other = step1::journal_directory(&root, sha256::Hash::hash(b"next source"));
    claim_workflow::prepare_directory(&other).unwrap();
    std::fs::write(other.join("intent.json"), b"{}").unwrap();
    assert_eq!(
        step1::discover_resumable(&root, &target, std::slice::from_ref(&record)),
        Some((sha256::Hash::hash(b"next source"), other))
    );
}

/// D19 never hides a live split: a journal in flight, in #625's dead end,
/// with a step-1 conflict, with its descriptors still on this device, that
/// can't be read, or whose lock is held is resumed even when the Cube's
/// `split_from` records its source and step 2.
/// CF: the check drops the dead end, the conflict or the deleted
/// descriptors.
#[test]
fn discovery_resumes_every_split_that_is_not_a_recorded_completion() {
    for placed in [Placed::InFlight, Placed::Step2DeadEnd, Placed::Conflict] {
        let (_temp, root) = journal_root();
        let (target, digest, directory, record) = place_journal(&root, placed);
        assert!(
            !step1::is_recorded_complete(
                &directory,
                &target,
                digest,
                std::slice::from_ref(&record)
            ),
            "{:?}",
            placed
        );
        assert_eq!(
            step1::discover_resumable(&root, &target, std::slice::from_ref(&record)),
            Some((digest, directory)),
            "{:?}",
            placed
        );
    }
    // Recorded and seen, but its descriptors are still here: a completion
    // written before its deletion failed is finished from Reconcile.
    let (_temp, root) = journal_root();
    let journal = Journal::returned(true);
    let directory = step1::journal_directory(&root, journal.digest());
    claim_workflow::prepare_directory(&directory).unwrap();
    std::fs::copy(
        journal.temp.0.join("intent.json"),
        directory.join("intent.json"),
    )
    .unwrap();
    let record = SplitFromRecord {
        descriptor_digest: journal.digest(),
        completed_height: 1_000,
        step2_txid: journal
            .lock()
            .recorded_split_step2()
            .unwrap()
            .compute_txid(),
    };
    assert!(!step1::is_recorded_complete(
        &directory,
        TARGET,
        journal.digest(),
        std::slice::from_ref(&record)
    ));
    // A completed one whose lock is held (another tab), or that can't be
    // read, is resumed too: the panel then reads it under a session.
    let (_temp, root) = journal_root();
    let (target, digest, directory, record) = place_journal(&root, Placed::Completed);
    let held = Controller::reopen_settling_blocking(
        &directory,
        &claim_workflow::split_identity(target.clone(), digest),
        context(),
    )
    .unwrap();
    assert!(!step1::is_recorded_complete(
        &directory,
        &target,
        digest,
        std::slice::from_ref(&record)
    ));
    drop(held);
    assert!(step1::is_recorded_complete(
        &directory,
        &target,
        digest,
        std::slice::from_ref(&record)
    ));
    std::fs::write(directory.join("intent.json"), b"{}").unwrap();
    assert!(!step1::is_recorded_complete(
        &directory,
        &target,
        digest,
        std::slice::from_ref(&record)
    ));
}

/// D15 after D19: a source this Cube completed stays refused (its record),
/// and a journal of it, completed or live, refuses it without one.
#[test]
fn a_completed_source_is_never_split_again() {
    for placed in [Placed::Completed, Placed::InFlight] {
        let (_temp, root) = journal_root();
        let (_, digest, _, record) = place_journal(&root, placed);
        assert_eq!(
            step1::second_split_refusal(&root, std::slice::from_ref(&record), digest),
            Some(step1::SOURCE_ALREADY_SPLIT)
        );
        assert_eq!(
            step1::second_split_refusal(&root, &[], digest),
            Some(step1::SOURCE_SPLIT_RECORDED)
        );
        // Another source is free.
        assert_eq!(
            step1::second_split_refusal(
                &root,
                std::slice::from_ref(&record),
                sha256::Hash::hash(b"another source")
            ),
            None
        );
    }
}
