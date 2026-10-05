//! Split (#568 B6c-2) child-process driver for the two-chain regtest
//! harness (`tests/test_btcb2_split_observation.py`). Never loaded by normal
//! builds: the `regtest-harness` feature and `--ignored` are both required.
//!
//! The Python parent owns the two pinned nodes, their indexers and a local
//! stand-in for Connect's Esplora and preflight routes; this child owns the
//! production Split services over that stand-in: `SplitProduction`,
//! `Coordinator::create_split` and its reopen, `SplitPreparation`, and after
//! step 2 `verify_split_step2_transaction`,
//! `record_split_step2_broadcast_intent` and `SplitStep2Reconciler::resume`.
//! Every observation is a real HTTP read of the real nodes' indexers.
//!
//! Synthetic: the foreign wallet's keys (the gate tests' P2PKH wallet), the
//! target Vault descriptor and its address reservation (no daemon), the
//! Connect account, and the step-2 fee, passed explicitly because Connect's
//! regtest fee estimates are empty. Step 2's bytes reach BTCB2 through the
//! parent: no step-2 transport is opened here.
use super::*;
use crate::services::claim_coordinator::{
    fork::split::step2::{SplitStep2Reconciler, Step1AfterStep2, SweepReconcile},
    split::SplitProduction,
};
use coincube_core::{
    foreign_split::{verify_split_step1_transaction, verify_split_step2_transaction},
    miniscript::bitcoin::consensus::encode::{deserialize_hex, serialize_hex},
};
use serde_json::Value;
use std::io::{BufRead, Write};

/// The account and session generation the synthetic Connect session uses.
const ACCOUNT: &str = "7";
const GENERATION: u64 = 1;
/// The bearer the parent's Connect stand-in requires for the anchor route.
const TOKEN: &str = "synthetic-regtest-only";
/// The production Claim check policy (36 h RDTS margin, 90 s observation
/// age).
const POLICY: CheckPolicy = crate::app::state::vault::claim::CHECK_POLICY;

fn emit(value: Value) {
    println!("SPLIT_SERVICE_JSON:{value}");
    std::io::stdout().flush().unwrap();
}
fn read(input: &mut impl BufRead) -> Value {
    let mut line = String::new();
    assert!(
        input.read_line(&mut line).unwrap() > 0,
        "parent closed command channel"
    );
    serde_json::from_str(&line).unwrap()
}
fn block(value: &Value) -> BlockRef {
    BlockRef {
        height: value["height"].as_u64().unwrap(),
        hash: value["hash"].as_str().unwrap().parse().unwrap(),
    }
}
fn after_json(after: Step1AfterStep2) -> Value {
    match after {
        Step1AfterStep2::Eligible => json!({"kind": "Eligible"}),
        Step1AfterStep2::Shallow { confirmations } => {
            json!({"kind": "Shallow", "confirmations": confirmations})
        }
        Step1AfterStep2::Remined {
            previous,
            confirmed,
        } => json!({"kind": "Remined", "previous": previous, "confirmed": confirmed}),
        Step1AfterStep2::InMempool => json!({"kind": "InMempool"}),
        Step1AfterStep2::Missing => json!({"kind": "Missing"}),
        Step1AfterStep2::Conflict(conflict) => {
            json!({"kind": "Conflict", "debug": format!("{conflict:?}")})
        }
        Step1AfterStep2::Unknown => json!({"kind": "Unknown"}),
    }
}

/// Everything the child keeps between commands. Each command opens its
/// service over the journal and drops it again, as a restart would; only
/// one holds the journal at a time.
struct Driver {
    directory: PathBuf,
    client: CoincubeClient,
    /// Kept alive: a dropped sender ends the session (`Revoked`).
    sender: watch::Sender<u64>,
    wallet: Wallet,
    coins: Vec<SplitCoin>,
    construction: SplitStep1,
    /// The signed step 1.
    signed: Transaction,
    fork_height: u64,
}
impl Driver {
    fn journal(&self) -> Value {
        std::fs::read(self.directory.join("intent.json"))
            .ok()
            .map(|bytes| serde_json::from_slice(&bytes).unwrap())
            .unwrap_or(Value::Null)
    }
    fn step1_production(&self) -> SplitProduction {
        SplitProduction::new(
            self.client.clone(),
            ACCOUNT.into(),
            GENERATION,
            self.sender.subscribe(),
            ChainId::Bitcoin,
        )
        .unwrap()
    }
    fn fork_production(&self) -> SplitForkProduction {
        SplitForkProduction::new(
            self.client.clone(),
            ACCOUNT.into(),
            GENERATION,
            self.sender.subscribe(),
        )
        .unwrap()
    }
    /// The recorded signed step 1 verified against the construction, as a
    /// restart verifies it.
    fn verified(&self) -> VerifiedSplitStep1 {
        verify_split_step1_transaction(
            &self.construction,
            &self.signed,
            &Secp256k1::verification_only(),
        )
        .unwrap()
    }
    /// The step-1 coordinator: created over a new journal, or reopened as
    /// the restart's resume reaches it.
    fn step1(&self, resume: bool) -> Result<Step1Coordinator, Error> {
        let production = self.step1_production();
        if resume {
            let context = production.context().clone();
            Step1Coordinator::open_split(
                &self.directory,
                TARGET.into(),
                &self.construction,
                self.verified(),
                self.fork_height,
                context,
                self.sender.subscribe(),
                Box::new(production),
                POLICY,
                true,
            )
        } else {
            Step1Coordinator::create_split(
                &self.directory,
                TARGET.into(),
                &self.construction,
                self.verified(),
                self.fork_height,
                production,
                POLICY,
            )
        }
    }
    fn preparation(&self) -> Result<SplitPreparation, Error> {
        SplitPreparation::resume(
            &self.directory,
            TARGET.into(),
            &self.construction,
            self.verified(),
            self.fork_height,
            self.fork_production(),
            POLICY,
        )
    }
    fn reconciler(&self) -> Result<SplitStep2Reconciler, Error> {
        SplitStep2Reconciler::resume(
            &self.directory,
            TARGET.into(),
            self.construction.source().digest(),
            self.fork_production(),
            POLICY,
        )
    }
    fn sweep_json(&self, action: &str, reconciled: Result<SweepReconcile, Error>) -> Value {
        match reconciled {
            Ok(r) => json!({"event": action, "status": format!("{:?}", r.status),
                "step2": format!("{:?}", r.step2), "step1": format!("{:?}", r.step1),
                "after_step2": after_json(r.after_step2), "journal": self.journal()}),
            Err(error) => {
                json!({"event": action, "error": format!("{error:?}"), "journal": self.journal()})
            }
        }
    }

    async fn run(&mut self, command: &Value) -> Value {
        let action = command["command"].as_str().unwrap();
        match action {
            // Review step 1 (Connect preflight) and submit it once through
            // the Connect route.
            "submit" => {
                let mut coordinator = self.step1(false).unwrap();
                let context = coordinator.context().clone();
                let review = coordinator.prepare_review(&context).await.unwrap();
                let route = review.snapshot().route.label();
                let outcome = coordinator.confirm_and_submit(review, &context).await;
                drop(coordinator);
                json!({"event": action, "route": route,
                    "outcome": format!("{outcome:?}"), "journal": self.journal()})
            }
            "reconcile_step1" => match self.step1(true) {
                Ok(mut coordinator) => {
                    let context = coordinator.context().clone();
                    let status = coordinator.reconcile(&context).await;
                    drop(coordinator);
                    json!({"event": action, "status": format!("{status:?}"),
                        "journal": self.journal()})
                }
                Err(error) => {
                    json!({"event": action, "open_error": format!("{error:?}"),
                        "journal": self.journal()})
                }
            },
            // The explicit, one-use acknowledgement of step 1 re-mined in
            // another block, on the step-1 coordinator, then a reconcile.
            "reconfirm_step1" => {
                let mut coordinator = self.step1(true).unwrap();
                let context = coordinator.context().clone();
                let review = coordinator.prepare_reconfirmation(&context).await;
                let inclusion = review.as_ref().ok().map(|review| {
                    let inclusion = review.inclusion();
                    json!({"previous": inclusion.previous, "confirmed": inclusion.confirmed})
                });
                let confirmed = match review {
                    Ok(review) => coordinator
                        .confirm_reconfirmation(review, &context)
                        .await
                        .map_err(|error| format!("{error:?}")),
                    Err(error) => Err(format!("{error:?}")),
                };
                let status = coordinator.reconcile(&context).await;
                drop(coordinator);
                json!({"event": action, "review": inclusion,
                    "confirmed": confirmed.err(), "status": format!("{status:?}"),
                    "journal": self.journal()})
            }
            // The six-confirmation gate before step 2.
            "check_signing" => {
                let mut preparation = match self.preparation() {
                    Ok(preparation) => preparation,
                    Err(error) => {
                        return json!({"event": action, "open_error": format!("{error:?}"),
                            "journal": self.journal()})
                    }
                };
                let context = preparation.context().clone();
                let checked = preparation.check_signing(&context).await;
                let tracked = checked
                    .as_ref()
                    .ok()
                    .map(|token| token.tracked_txid().to_string());
                let error = checked.err().map(|error| format!("{error:?}"));
                drop(preparation);
                json!({"event": action, "minted": error.is_none(), "tracked_txid": tracked,
                    "error": error, "journal": self.journal()})
            }
            // Step 2: target reserved (synthetic Vault, no daemon) and
            // proven fresh on both chains through Connect, built under a
            // live token with the explicit fee, signed, verified as a restart
            // verifies recorded bytes, and recorded with its submission
            // intent after another fresh check. Nothing is sent.
            "record_step2" => {
                let feerate = command["feerate"].as_u64().unwrap();
                let secp = Secp256k1::new();
                let mut preparation = self.preparation().unwrap();
                let context = preparation.context().clone();
                drop(preparation.check_signing(&context).await.unwrap());
                let polls = Arc::new(AtomicUsize::new(0));
                let index = preparation
                    .reserve_target(
                        &context,
                        &vault(),
                        reserved(&vault(), INDEX, &polls),
                        Duration::from_secs(5),
                    )
                    .await
                    .unwrap();
                preparation.prove_target(&context, &vault()).await.unwrap();
                let token = preparation.check_signing(&context).await.unwrap();
                let psbt = preparation
                    .construct_step2(&context, token, self.coins.clone(), &Fees(Some(feerate)))
                    .await
                    .unwrap();
                let mut signed = psbt.clone();
                signed.sign(&self.wallet.signer, &secp).unwrap();
                let construction = preparation.step2.clone().unwrap();
                let finalized = finalize_split_step2(
                    &construction,
                    &self.coins,
                    &self.wallet.source,
                    &signed,
                    &secp,
                )
                .unwrap();
                let verified =
                    verify_split_step2_transaction(&construction, finalized.transaction(), &secp)
                        .unwrap();
                // The record needs a fresh assessment of its own.
                drop(preparation.check_signing(&context).await.unwrap());
                let now = preparation.services.source().now();
                preparation
                    .controller
                    .record_split_step2_broadcast_intent(
                        &context,
                        &verified,
                        POLICY.observations,
                        now,
                    )
                    .unwrap();
                drop(preparation);
                let tx = verified.transaction();
                json!({"event": action, "target_index": index,
                    "target_script": construction.target().to_hex_string(),
                    "step2_txid": tx.compute_txid(), "step2_wtxid": tx.compute_wtxid(),
                    "step2_raw": serialize_hex(tx), "journal": self.journal()})
            }
            "reconcile_step2" => match self.reconciler() {
                Ok(mut reconciler) => {
                    let context = self.fork_production().context().clone();
                    let reconciled = reconciler.reconcile_sweep(&context).await;
                    drop(reconciler);
                    self.sweep_json(action, reconciled)
                }
                Err(error) => {
                    json!({"event": action, "open_error": format!("{error:?}"),
                        "journal": self.journal()})
                }
            },
            // Completion evidence is only minted, never persisted here.
            "check_completion" => {
                let mut reconciler = self.reconciler().unwrap();
                let context = self.fork_production().context().clone();
                let evidence = reconciler.check_completion(&context).await;
                let result = match &evidence {
                    Ok(Some(evidence)) => json!({"txid": evidence.txid(),
                        "block": evidence.block()}),
                    Ok(None) => Value::Null,
                    Err(error) => json!({"error": format!("{error:?}")}),
                };
                drop(evidence);
                drop(reconciler);
                json!({"event": action, "completion": result, "journal": self.journal()})
            }
            // O1 after step 2: the reconciler's one-use acknowledgement.
            "reconfirm_after_step2" => {
                let mut reconciler = self.reconciler().unwrap();
                let context = self.fork_production().context().clone();
                let review = reconciler.prepare_step1_reconfirmation(&context).await;
                let inclusion = review.as_ref().ok().map(|review| {
                    let inclusion = review.inclusion();
                    json!({"previous": inclusion.previous, "confirmed": inclusion.confirmed})
                });
                let confirmed = match review {
                    Ok(review) => reconciler
                        .confirm_step1_reconfirmation(review, &context)
                        .await
                        .map_err(|error| format!("{error:?}")),
                    Err(error) => Err(format!("{error:?}")),
                };
                drop(reconciler);
                json!({"event": action, "review": inclusion, "confirmed": confirmed.err(),
                    "journal": self.journal()})
            }
            _ => panic!("unknown command {}", action),
        }
    }
}

/// Invoked directly by the Python integration parent with `--ignored
/// --nocapture`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the disposable two-chain Python parent"]
async fn split_service_regtest_driver() {
    assert_eq!(
        std::env::var("SPLIT_SERVICE_REGTEST_CHILD").as_deref(),
        Ok("1")
    );
    let mut input = std::io::BufReader::new(std::io::stdin());
    let wallet = wallet();
    emit(json!({"event": "descriptors",
        "external": wallet.source.external().to_string(),
        "internal": wallet.source.internal().unwrap().to_string()}));
    let init = read(&mut input);
    let root = PathBuf::from(init["root"].as_str().unwrap());
    assert!(
        root.read_dir().unwrap().next().is_none(),
        "use a fresh synthetic root"
    );
    let directory = root.join("split");
    std::fs::create_dir(&directory).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let fork_height = init["fork_height"].as_u64().unwrap();
    // The funded coins as each node reported them: pre-fork, so one block
    // on both chains.
    let coins: Vec<SplitCoin> = init["coins"]
        .as_array()
        .unwrap()
        .iter()
        .map(|coin| {
            let previous: Transaction =
                deserialize_hex(coin["previous"].as_str().unwrap()).unwrap();
            let vout = u32::try_from(coin["vout"].as_u64().unwrap()).unwrap();
            let (branch, descriptor) = match coin["branch"].as_str().unwrap() {
                "external" => (SplitBranch::External, wallet.source.external()),
                "internal" => (SplitBranch::Internal, wallet.source.internal().unwrap()),
                other => panic!("unknown branch {}", other),
            };
            let index = u32::try_from(coin["index"].as_u64().unwrap()).unwrap();
            // The parent funded exactly this wallet's script.
            assert_eq!(
                previous.output[vout as usize].script_pubkey,
                descriptor
                    .at_derivation_index(index)
                    .unwrap()
                    .script_pubkey()
            );
            let confirmed = Some(block(&coin["block"]));
            SplitCoin {
                outpoint: OutPoint::new(previous.compute_txid(), vout),
                branch,
                index,
                previous,
                bitcoin_block: confirmed,
                btcb2_block: confirmed,
            }
        })
        .collect();
    let tip = u32::try_from(init["bitcoin_tip_height"].as_u64().unwrap()).unwrap();
    let construction = create_split_step1(
        &SplitInputs {
            chain: ChainId::Bitcoin,
            source: &wallet.source,
            coins: &coins,
            fork_height,
            destination: 5,
        },
        init["feerate"].as_u64().unwrap(),
        LockTime::from_height(tip).unwrap(),
        tip,
        init["fork_marker"].as_str().unwrap().parse().unwrap(),
    )
    .unwrap();
    let signed = sign(&construction, &wallet).transaction().clone();
    let mut client = CoincubeClient::for_test(init["bridge"].as_str().unwrap().to_owned());
    client.set_token(TOKEN);
    let (sender, _) = watch::channel(GENERATION);
    let mut driver = Driver {
        directory,
        client,
        sender,
        wallet,
        coins,
        construction,
        signed,
        fork_height,
    };
    emit(json!({"event": "ready",
        "step1_txid": driver.signed.compute_txid(),
        "step1_wtxid": driver.signed.compute_wtxid(),
        "step1_raw": serialize_hex(&driver.signed),
        "claimed": driver.construction.claimed_prevouts().iter().map(|o| o.to_string()).collect::<Vec<_>>(),
        "journal": driver.journal()}));
    loop {
        let command = read(&mut input);
        if command["command"] == "quit" {
            break;
        }
        let result = driver.run(&command).await;
        emit(result);
    }
}
