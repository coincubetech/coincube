//! The managed node's flavour ledger, and the record every managed-node start
//! leaves in it.
//!
//! The managed Bitcoin node runs as either Bitcoin Core or Bitcoin Knots — its
//! *flavour*. Which one the user configured and which one was last observed
//! running live in a sidecar beside the node's datadir ([`ManagedNodeState`]);
//! [`reconcile_after_start`] writes the observed half on every start path.
//!
//! # Historical note
//!
//! Until RDTS sunset PR 4 (#510) this module also carried the BIP-110 / RDTS
//! fork-repair machinery: a planner over the node's chain facts, a Knots → Core
//! flag-clearing repair, a Core → Knots rewind-and-replay, the sanctioned-rollback
//! authorisation every Vault's poller consulted, and the one-shot repair notice.
//! No shipped build enforced the deployment by then, so the whole of it was
//! deleted rather than left dormant. The ledger fields it wrote
//! (`last_run_enforced_rdts`, `repair_notice_pending`, `rewind`,
//! `sanctioned_rollback`) are ignored when read from an older sidecar and dropped
//! by the next write.

use std::{
    io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::{
    dir::CoincubeDirectory,
    node::bitcoind::{NodeFlavor, NodeIdentity, ObservedBuild},
};

// ---------------------------------------------------------------------------
// The flavour ledger
// ---------------------------------------------------------------------------

/// Persistent record of how the managed node is configured and how it last ran.
///
/// [`Self::last_run_flavor`] deliberately records the flavour **observed** from
/// the started node's `getnetworkinfo.subversion`, not the flavour that was
/// configured or requested. The configured value is not reliable on its own:
/// `select_managed_bitcoind_exe` falls back to the other flavour's binary when the
/// preferred one isn't installed, and the installer and loader can start the node
/// without going through the settings switch at all.
///
/// A missing, stale or corrupt sidecar costs a fallback, nothing more: the node
/// settings card shows the configured flavour instead of the observed one, and
/// [`crate::node::bitcoind::configured_managed_flavor`] reaches for the observed
/// one only when nothing was ever configured.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagedNodeState {
    /// Flavour the node was observed running as, last time it started.
    pub last_run_flavor: Option<NodeFlavor>,
    /// Flavour the node is *configured* to run as — what the user picked, as
    /// opposed to [`Self::last_run_flavor`], which is what actually came up.
    ///
    /// This one has to be durable because there is nowhere else to put it:
    /// bitcoind rejects options it doesn't recognise, so the managed
    /// `bitcoin.conf` cannot carry a flavour key of ours, and the marker it was
    /// previously inferred from (`consensusrules=rdts`) is no longer written.
    /// Written by every surface that writes the managed config; read by
    /// [`crate::node::bitcoind::configured_managed_flavor`].
    #[serde(default)]
    pub configured_flavor: Option<NodeFlavor>,
}

impl ManagedNodeState {
    /// Sidecar path: `<datadir>/bitcoind/managed_node_state.json`.
    ///
    /// Alongside `inbound_tor.json` in the managed-node directory, not in a vault
    /// datadir: the managed node is shared by every vault, and vault datadirs are
    /// removed wholesale on delete.
    pub fn path(coincube_datadir: &CoincubeDirectory) -> PathBuf {
        Self::path_for(
            coincube_datadir,
            crate::node::bitcoind::NodeChainFamily::Bitcoin,
        )
    }

    /// Sidecar path for a chain family's managed node:
    /// `<datadir>/<family root>/managed_node_state.json`. The Bitcoin ledger
    /// ([`Self::path`]) and a Bitcoin Blake2b node's ledger are different files
    /// under different roots — the ledger is per node directory, never a
    /// datadir-wide singleton.
    pub fn path_for(
        coincube_datadir: &CoincubeDirectory,
        family: crate::node::bitcoind::NodeChainFamily,
    ) -> PathBuf {
        crate::node::bitcoind::internal_bitcoind_directory_for(coincube_datadir, family)
            .join("managed_node_state.json")
    }

    /// Load the ledger, distinguishing "there is no sidecar yet" from "there is one
    /// and we could not read it".
    ///
    /// Only a missing file yields the default. A read error or a corrupt file is an
    /// error, so the writers below can leave an unreadable sidecar where it is
    /// rather than overwrite it with a default.
    pub fn try_load(coincube_datadir: &CoincubeDirectory) -> io::Result<Self> {
        let path = Self::path(coincube_datadir);
        let contents = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e),
        };
        serde_json::from_str(&contents).map_err(io::Error::other)
    }

    /// Fail-safe view of [`Self::try_load`], where "unknown" is a safe answer: the
    /// next start just records the truth rather than acting on a guess.
    pub fn load(coincube_datadir: &CoincubeDirectory) -> Self {
        Self::try_load(coincube_datadir).unwrap_or_else(|e| {
            let path = Self::path(coincube_datadir);
            warn!("unreadable managed-node state at {path:?} ({e}); treating as unknown");
            Self::default()
        })
    }

    /// Persist the ledger via a temp file and a rename, so an interrupted write
    /// cannot leave a half-written sidecar that would later parse as garbage.
    /// (The `inbound_tor.json` precedent writes in place; this one is on the
    /// startup path of every vault, so it is worth the extra care.)
    pub fn save(&self, coincube_datadir: &CoincubeDirectory) -> io::Result<()> {
        let path = Self::path(coincube_datadir);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        write_atomically(&path, json.as_bytes())
    }

    /// Record the flavour the node was just observed running as, leaving the
    /// configured flavour as it is.
    ///
    /// Reads with `try_load` rather than `load`: defaulting on an unreadable sidecar
    /// would overwrite whatever it held with a fabricated state. Skipping the update
    /// is the safe failure — the next start records the truth again.
    pub fn record_run(coincube_datadir: &CoincubeDirectory, observed: ObservedBuild) {
        let mut state = match Self::try_load(coincube_datadir) {
            Ok(state) => state,
            Err(e) => {
                warn!("not recording the managed-node flavour: state unreadable ({e})");
                return;
            }
        };
        state.last_run_flavor = Some(observed.flavor);
        if let Err(e) = state.save(coincube_datadir) {
            warn!("could not record managed-node flavour: {e}");
        }
    }

    /// Record the flavour the managed node is configured to run as.
    ///
    /// Called wherever the managed `bitcoin.conf` is written, because that file
    /// can no longer hold the answer itself. Same `try_load` discipline as
    /// [`Self::record_run`]: a sidecar we cannot read is left alone rather than
    /// overwritten with a default.
    pub fn record_configured(coincube_datadir: &CoincubeDirectory, flavor: NodeFlavor) {
        let mut state = match Self::try_load(coincube_datadir) {
            Ok(state) => state,
            Err(e) => {
                warn!("not recording the configured managed-node flavour: state unreadable ({e})");
                return;
            }
        };
        if state.configured_flavor == Some(flavor) {
            return;
        }
        state.configured_flavor = Some(flavor);
        if let Err(e) = state.save(coincube_datadir) {
            warn!("could not record the configured managed-node flavour: {e}");
        }
    }
}

/// Write `contents` to `path` through a sibling temp file, flushing before the
/// rename so the visible file is either the old one or the complete new one.
///
/// The directory is fsynced after the rename as well. Without that, the file's
/// contents are durable but the directory entry pointing at them may not be, so a
/// crash can lose the rename and with it the record.
fn write_atomically(path: &Path, contents: &[u8]) -> io::Result<()> {
    use std::io::Write;

    let tmp = path.with_extension("tmp");
    {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(contents)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;

    // Unix only: Windows has no equivalent (a directory can't be opened as a file
    // without backup semantics), and NTFS metadata journalling makes the rename
    // durable there anyway.
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The start-time record
// ---------------------------------------------------------------------------

/// Observe how the managed node came up and record it in the flavour ledger.
///
/// Called from every managed-node start path, because `Bitcoind::maybe_start` is
/// the one funnel the loader, the installer, and the settings switch all pass
/// through. `observed` must come from the node's own `getnetworkinfo.subversion`
/// — not from configuration, which can disagree with what actually launched.
///
/// Best-effort: a node that just started is more important than this record, so
/// a failure to write it is logged and swallowed rather than blocking startup.
///
/// Two refusals come first, in this order, and both return before the ledger is
/// so much as read:
///
/// 1. A Bitcoin Blake2b chain, or a Blake2b provider reported on any chain. The
///    ledger under `bitcoind/` is the *Bitcoin* node's, and a Blake2b start must
///    neither read nor rewrite it (`docs/BTCB2_MANAGED_NODE.md`).
/// 2. An unsettled node identity. The node is already up and syncing by then;
///    declining to record costs a retry on the next start, which is what the
///    durable record is for.
pub fn reconcile_after_start(
    coincube_datadir: &CoincubeDirectory,
    identity: &NodeIdentity,
    chain: crate::chain::ChainId,
    observed: ObservedBuild,
) {
    // The ledger below is the *Bitcoin* node's (`bitcoind/`). Leave before
    // touching it.
    if chain.is_blake2b()
        || observed.flavor.chain_family() != crate::node::bitcoind::NodeChainFamily::Bitcoin
    {
        tracing::debug!(
            "not recording the managed node's flavour for {}: not a Bitcoin chain",
            chain
        );
        return;
    }
    if !identity.permits_chain_repair() {
        warn!(
            "not recording the managed node's flavour: its identity is not established \
             yet. It will be recorded on the next start."
        );
        return;
    }
    ManagedNodeState::record_run(coincube_datadir, observed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::ChainId;
    use crate::node::bitcoind::{internal_bitcoind_directory_for, NodeChainFamily};

    fn a_temp_datadir(name: &str) -> (std::path::PathBuf, CoincubeDirectory) {
        let dir = std::env::temp_dir().join(format!(
            "coincube-{name}-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        (dir.clone(), CoincubeDirectory::new(dir))
    }

    fn ledger_bytes(root: &CoincubeDirectory) -> Vec<u8> {
        std::fs::read(ManagedNodeState::path(root)).unwrap()
    }

    // A Bitcoin Blake2b node is outside this module's remit, and is decided before
    // any Bitcoin fact is looked at: the ledger under `bitcoind/` is the Bitcoin
    // node's, and a Blake2b start must neither read nor rewrite it. Proven by
    // contrast, not by shape: the identical observation on the Bitcoin chain
    // reaches `record_run` and changes the ledger bytes, while on either Blake2b
    // chain — or for the Blake2b provider reported on the Bitcoin chain — the
    // function returns with the ledger byte-identical and the Blake2b root never
    // created.
    #[test]
    fn a_blake2b_chain_is_skipped_before_any_bitcoin_fact_is_inspected() {
        let (dir, root) = a_temp_datadir("btcb2-reconcile");
        // A ledger that any Bitcoin start would advance: it remembers Knots, and
        // the node now reports Core.
        ManagedNodeState {
            last_run_flavor: Some(NodeFlavor::Knots),
            configured_flavor: Some(NodeFlavor::Core),
        }
        .save(&root)
        .unwrap();
        let before = ledger_bytes(&root);
        let observed = ObservedBuild::assumed(NodeFlavor::Core);

        for chain in [ChainId::BitcoinBlake2b, ChainId::BitcoinBlake2bTestnet4] {
            // The chain alone decides it, whatever the node reports itself as …
            reconcile_after_start(&root, &NodeIdentity::Stable, chain, observed);
            // … and so does the Blake2b provider itself.
            reconcile_after_start(
                &root,
                &NodeIdentity::Stable,
                chain,
                ObservedBuild::assumed(NodeFlavor::KnotsBlake2b),
            );
            assert_eq!(
                ledger_bytes(&root),
                before,
                "{chain}: the Bitcoin ledger was touched"
            );
        }
        // A Blake2b provider reported on the Bitcoin chain is refused the same way
        // rather than recorded as a Bitcoin run.
        reconcile_after_start(
            &root,
            &NodeIdentity::Stable,
            ChainId::Bitcoin,
            ObservedBuild::assumed(NodeFlavor::KnotsBlake2b),
        );
        assert_eq!(ledger_bytes(&root), before);
        // And nothing was created under the Blake2b root.
        assert!(!ManagedNodeState::path_for(&root, NodeChainFamily::BitcoinBlake2b).exists());
        assert!(!internal_bitcoind_directory_for(&root, NodeChainFamily::BitcoinBlake2b).exists());

        // The contrast that gives the refusals above something to be measured
        // against: the identical observation on the Bitcoin chain is recorded.
        reconcile_after_start(&root, &NodeIdentity::Stable, ChainId::Bitcoin, observed);
        assert_ne!(
            ledger_bytes(&root),
            before,
            "a Bitcoin start must reach record_run"
        );
        let recorded = ManagedNodeState::load(&root);
        assert_eq!(recorded.last_run_flavor, Some(NodeFlavor::Core));
        assert_eq!(recorded.configured_flavor, Some(NodeFlavor::Core));

        let _ = std::fs::remove_dir_all(&dir);
    }

    // The ledger is only advanced against a settled node identity, exactly as it
    // was while the repair machinery gated on the same predicate: a start that
    // could not settle the identity leaves the record alone, and the next start
    // writes it. Dropping the guard would be a behaviour change, not a deletion.
    #[test]
    fn an_unsettled_identity_leaves_the_ledger_alone() {
        let (dir, root) = a_temp_datadir("unstable-identity");
        ManagedNodeState {
            last_run_flavor: Some(NodeFlavor::Knots),
            ..Default::default()
        }
        .save(&root)
        .unwrap();
        let before = ledger_bytes(&root);
        let observed = ObservedBuild::assumed(NodeFlavor::Core);

        reconcile_after_start(&root, &NodeIdentity::Unstable, ChainId::Bitcoin, observed);
        assert_eq!(ledger_bytes(&root), before);

        // Not simply always closed: the same start under a settled identity records.
        reconcile_after_start(&root, &NodeIdentity::Stable, ChainId::Bitcoin, observed);
        assert_eq!(
            ManagedNodeState::load(&root).last_run_flavor,
            Some(NodeFlavor::Core)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // A sidecar written before the repair machinery was removed carries fields this
    // build no longer has. It must still read — the two flavours in it are the
    // ones that matter — and the next write drops what it no longer understands
    // rather than carrying it forward as dead weight.
    #[test]
    fn a_pre_sunset_sidecar_still_reads_and_is_rewritten_without_the_repair_fields() {
        let (dir, root) = a_temp_datadir("pre-sunset-sidecar");
        let path = ManagedNodeState::path(&root);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // The shape the last release to write the repair fields serialised, with
        // every one of them populated.
        std::fs::write(
            &path,
            br#"{
  "last_run_flavor": "Knots",
  "last_run_enforced_rdts": true,
  "configured_flavor": "Core",
  "repair_notice_pending": true,
  "rewind": {
    "invalidated_hash": "0000000000000000000000000000000000000000000000000000000000000000",
    "floor_height": 961631,
    "target_height": 966631
  },
  "sanctioned_rollback": {
    "hash": "000000000000000000000000000000000000000000000000000000000000002a",
    "height": 961631,
    "node_addr": "127.0.0.1:8332",
    "node_credentials": "abc",
    "confirmed": true,
    "operation_id": "operation-A"
  }
}"#,
        )
        .unwrap();

        let loaded = ManagedNodeState::try_load(&root).expect("an older sidecar still reads");
        assert_eq!(
            loaded,
            ManagedNodeState {
                last_run_flavor: Some(NodeFlavor::Knots),
                configured_flavor: Some(NodeFlavor::Core),
            }
        );

        ManagedNodeState::record_run(&root, ObservedBuild::assumed(NodeFlavor::Core));
        let rewritten = std::fs::read_to_string(&path).unwrap();
        for retired in [
            "last_run_enforced_rdts",
            "repair_notice_pending",
            "rewind",
            "sanctioned_rollback",
        ] {
            assert!(
                !rewritten.contains(retired),
                "{} survived the rewrite:\n{}",
                retired,
                rewritten
            );
        }
        let reloaded = ManagedNodeState::load(&root);
        assert_eq!(reloaded.last_run_flavor, Some(NodeFlavor::Core));
        assert_eq!(reloaded.configured_flavor, Some(NodeFlavor::Core));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_ledger_round_trips_and_fails_safe() {
        let dir = std::env::temp_dir().join(format!(
            "coincube-revalidate-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let datadir = CoincubeDirectory::new(dir.clone());

        // Absent sidecar reads as "never observed", not as an error.
        assert_eq!(ManagedNodeState::load(&datadir).last_run_flavor, None);

        ManagedNodeState::record_run(&datadir, ObservedBuild::assumed(NodeFlavor::Knots));
        assert_eq!(
            ManagedNodeState::load(&datadir).last_run_flavor,
            Some(NodeFlavor::Knots),
        );

        // Overwriting must replace, not append or corrupt.
        ManagedNodeState::record_run(&datadir, ObservedBuild::assumed(NodeFlavor::Core));
        assert_eq!(
            ManagedNodeState::load(&datadir).last_run_flavor,
            Some(NodeFlavor::Core),
        );

        // A corrupt sidecar fails safe to "unknown" for the flavour ledger...
        std::fs::write(ManagedNodeState::path(&datadir), b"{ not json").unwrap();
        assert_eq!(ManagedNodeState::load(&datadir).last_run_flavor, None);

        // ...but `try_load` must report it, so a writer does not replace a file it
        // could not read.
        assert!(ManagedNodeState::try_load(&datadir).is_err());

        // A write must not clobber state it could not read: better to skip the
        // update than to overwrite it with a default.
        ManagedNodeState::record_run(&datadir, ObservedBuild::assumed(NodeFlavor::Core));
        assert!(ManagedNodeState::try_load(&datadir).is_err());

        // A *missing* sidecar is different: that genuinely means "nothing yet".
        std::fs::remove_file(ManagedNodeState::path(&datadir)).unwrap();
        assert_eq!(
            ManagedNodeState::try_load(&datadir).unwrap(),
            ManagedNodeState::default()
        );

        // No stray temp file is left behind by the atomic write.
        assert!(!ManagedNodeState::path(&datadir)
            .with_extension("tmp")
            .exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
