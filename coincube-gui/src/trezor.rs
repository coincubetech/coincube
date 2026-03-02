use std::sync::Mutex;

use async_trait::async_trait;
use async_hwi::{AddressScript, DeviceKind, Error as HWIError, Version, HWI};
use bitcoin::{
    bip32::{DerivationPath, Fingerprint, Xpub},
    psbt::Psbt,
    Network,
};
use trezor_client::{InputScriptType, TrezorResponse};
use trezor_client::protos::tx_request::RequestType as TxRequestType;

/// A Trezor hardware wallet device.
///
/// Wraps `trezor_client::Trezor` and implements the `async_hwi::HWI` trait so it can
/// be used in the same `Arc<dyn HWI + Send + Sync>` slot as every other hardware wallet.
pub struct TrezorDevice {
    inner: Mutex<trezor_client::Trezor>,
    network: Network,
}

impl std::fmt::Debug for TrezorDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrezorDevice")
            .field("network", &self.network)
            .finish_non_exhaustive()
    }
}

// Safety: trezor_client::Trezor contains a `Box<dyn Transport>` that does not carry a `Send`
// bound in its trait definition.  The concrete transport used in practice (WebUsbTransport /
// UdpTransport) wraps `rusb::DeviceHandle`, which is designed for multi-threaded use (libusb is
// internally thread-safe when a single handle is accessed serially).  The `Mutex<Trezor>` here
// guarantees that only one thread accesses the device at a time, satisfying the invariant.
unsafe impl Send for TrezorDevice {}
unsafe impl Sync for TrezorDevice {}

impl TrezorDevice {
    /// Connect and initialise a Trezor device, caching firmware features for later use.
    ///
    /// `trezor-client` only knows the coin names "Bitcoin" and "Testnet" (testnet3).
    /// Testnet4 uses the same address format, derivation paths, and coin name as Testnet,
    /// so we normalise Testnet4 → Testnet before passing any network to the trezor-client.
    pub fn new(mut trezor: trezor_client::Trezor, network: Network) -> Result<Self, HWIError> {
        let trezor_network = normalize_network(network);
        trezor
            .init_device(None)
            .map_err(|e| HWIError::Device(e.to_string()))?;
        Ok(Self {
            inner: Mutex::new(trezor),
            network: trezor_network,
        })
    }
}

/// Map networks that `trezor-client` does not know about to their closest supported equivalent.
/// Testnet4 is structurally identical to Testnet (same address format, same derivation paths,
/// same coin name in Trezor firmware), so it maps to `Network::Testnet`.
fn normalize_network(network: Network) -> Network {
    match network {
        Network::Testnet4 => Network::Testnet,
        other => other,
    }
}

/// Resolve a `TrezorResponse` to its final `Ok` value, automatically acknowledging button
/// confirmations and passphrase-on-device prompts.
///
/// Returns an error for PIN-matrix requests (the user must unlock the device first) and for
/// explicit `Failure` responses from the firmware.
fn resolve_trezor<T, R>(mut resp: TrezorResponse<'_, T, R>) -> Result<T, HWIError>
where
    R: trezor_client::TrezorMessage,
{
    loop {
        resp = match resp {
            TrezorResponse::Ok(val) => return Ok(val),
            TrezorResponse::Failure(f) => {
                let msg = f.message().to_string();
                if msg.to_ascii_lowercase().contains("cancel")
                    || msg.to_ascii_lowercase().contains("refus")
                {
                    return Err(HWIError::UserRefused);
                }
                return Err(HWIError::Device(msg));
            }
            TrezorResponse::ButtonRequest(btn) => {
                btn.ack().map_err(|e| HWIError::Device(e.to_string()))?
            }
            TrezorResponse::PassphraseRequest(p) => {
                // Request the user to enter the passphrase on the Trezor device itself.
                p.ack(true).map_err(|e| HWIError::Device(e.to_string()))?
            }
            TrezorResponse::PinMatrixRequest(_) => {
                return Err(HWIError::Device(
                    "Trezor PIN entry required; please unlock the device first".to_string(),
                ))
            }
        };
    }
}

#[async_trait]
impl HWI for TrezorDevice {
    fn device_kind(&self) -> DeviceKind {
        // async_hwi 0.0.29 does not contain a Trezor variant in DeviceKind.
        // This method is called only by HardwareWallet::new() which is used for Specter devices;
        // Trezor hardware wallets are constructed directly with HWKind::Trezor and never reach
        // this code path.  We return Specter as a harmless placeholder.
        DeviceKind::Specter
    }

    async fn get_version(&self) -> Result<Version, HWIError> {
        let guard = self.inner.lock().unwrap();
        guard
            .features()
            .map(|f| Version {
                major: f.major_version() as u32,
                minor: f.minor_version() as u32,
                patch: f.patch_version() as u32,
                prerelease: None,
            })
            .ok_or_else(|| HWIError::Device("Could not read Trezor firmware version".to_string()))
    }

    async fn get_master_fingerprint(&self) -> Result<Fingerprint, HWIError> {
        let master_path = DerivationPath::master();
        let mut guard = self.inner.lock().unwrap();
        let resp = guard
            .get_public_key(&master_path, InputScriptType::SPENDADDRESS, self.network, false)
            .map_err(|e| HWIError::Device(e.to_string()))?;
        let xpub: Xpub = resolve_trezor(resp)?;
        Ok(xpub.fingerprint())
    }

    async fn get_extended_pubkey(&self, path: &DerivationPath) -> Result<Xpub, HWIError> {
        let mut guard = self.inner.lock().unwrap();
        let resp = guard
            .get_public_key(path, InputScriptType::SPENDADDRESS, self.network, false)
            .map_err(|e| HWIError::Device(e.to_string()))?;
        resolve_trezor(resp)
    }

    async fn register_wallet(
        &self,
        _name: &str,
        _policy: &str,
    ) -> Result<Option<[u8; 32]>, HWIError> {
        // Trezor does not require wallet registration or HMAC tokens.
        Ok(None)
    }

    async fn is_wallet_registered(
        &self,
        _name: &str,
        _policy: &str,
    ) -> Result<bool, HWIError> {
        // Trezor does not require wallet registration.
        Ok(true)
    }

    async fn display_address(&self, script: &AddressScript) -> Result<(), HWIError> {
        match script {
            AddressScript::P2TR(path) => {
                let mut guard = self.inner.lock().unwrap();
                let resp = guard
                    .get_address(path, InputScriptType::SPENDTAPROOT, self.network, true)
                    .map_err(|e| HWIError::Device(e.to_string()))?;
                resolve_trezor(resp)?;
                Ok(())
            }
            AddressScript::Miniscript { .. } => {
                // Trezor miniscript address display requires PSBT-based flows not yet
                // implemented here.
                Err(HWIError::UnimplementedMethod)
            }
        }
    }

    async fn sign_tx(&self, tx: &mut Psbt) -> Result<(), HWIError> {
        let mut guard = self.inner.lock().unwrap();

        // Get the Trezor's master fingerprint so we can find its key in the PSBT.
        // trezor-client 0.1.5's ack_psbt only sets address_n when bip32_derivation
        // has exactly 1 entry (single-sig), and never checks tap_key_origins (Taproot).
        // We replace the input-ack step with our own builder that handles both cases.
        let fp_resp = guard
            .get_public_key(&DerivationPath::master(), InputScriptType::SPENDADDRESS, self.network, false)
            .map_err(|e| HWIError::Device(e.to_string()))?;
        let master_xpub: Xpub = resolve_trezor(fp_resp)?;
        let trezor_fp = master_xpub.fingerprint();

        // Detect Taproot script-path inputs early and return a clear error.
        // Coincube wallets use a NUMS (unspendable) internal key, so every spend is
        // a script-path (tapscript) spend.  trezor-client 0.1.5's sign_tx protocol
        // only supports key-path BIP86 Taproot — it lacks the proto fields required
        // for tapscript (leaf script, control block).  Attempting to sign produces
        // a cryptic "Input does not match scriptPubKey" firmware error, so we fail
        // early with a descriptive message instead.
        let has_taproot_script_path = tx.inputs.iter().zip(tx.unsigned_tx.input.iter()).any(
            |(psbt_in, _)| {
                let is_taproot = psbt_in
                    .witness_utxo
                    .as_ref()
                    .map(|u| u.script_pubkey.is_p2tr())
                    .unwrap_or(false);
                let trezor_in_tap_origins = psbt_in
                    .tap_key_origins
                    .values()
                    .any(|(_, ks)| ks.0 == trezor_fp);
                is_taproot && trezor_in_tap_origins
            },
        );
        if has_taproot_script_path {
            return Err(HWIError::UnimplementedMethod);
        }

        // Initiate the signing exchange.
        let initial_resp = guard
            .sign_tx(tx, self.network)
            .map_err(|e| HWIError::Device(e.to_string()))?;

        // Resolve the initial response (may require button confirmation).
        let mut progress = resolve_trezor(initial_resp)?;

        // Drive the interactive signing protocol until the firmware signals completion.
        while !progress.finished() {
            // Extract what we need from the current request before consuming `progress`.
            let (req_type, has_tx_hash, input_index) = {
                let req = progress.tx_request();
                (
                    req.request_type(),
                    req.details.has_tx_hash(),
                    req.details.request_index() as usize,
                )
            };

            // For input requests on the signing tx (not dependent txs), build a
            // custom TxAck that correctly sets address_n and script_type.
            let next_resp =
                if req_type == TxRequestType::TXINPUT && !has_tx_hash {
                    let ack = build_signing_input_ack(input_index, tx, trezor_fp)?;
                    progress.ack_msg(ack).map_err(|e| HWIError::Device(e.to_string()))?
                } else {
                    progress
                        .ack_psbt(&*tx, self.network)
                        .map_err(|e| HWIError::Device(e.to_string()))?
                };
            progress = resolve_trezor(next_resp)?;
        }

        Ok(())
    }
}

/// Build a `TxAck` for a signing-tx input request that correctly populates `address_n` and
/// `script_type` regardless of how many signers are in the PSBT.
///
/// `trezor-client 0.1.5`'s built-in `ack_psbt` only sets `address_n` when
/// `bip32_derivation.len() == 1` (single-sig) and never inspects `tap_key_origins` (Taproot).
/// This function fixes both gaps by searching for the Trezor's fingerprint in the appropriate
/// PSBT field for the script type being spent.
fn build_signing_input_ack(
    input_index: usize,
    psbt: &Psbt,
    trezor_fp: Fingerprint,
) -> Result<trezor_client::protos::TxAck, HWIError> {
    use trezor_client::protos::{
        tx_ack::{transaction_type::TxInputType, TransactionType},
        TxAck,
    };

    let input = psbt
        .unsigned_tx
        .input
        .get(input_index)
        .ok_or_else(|| HWIError::Device(format!("PSBT missing input at index {}", input_index)))?;
    let psbt_input = psbt.inputs.get(input_index).ok_or_else(|| {
        HWIError::Device(format!("PSBT missing input metadata at index {}", input_index))
    })?;

    // Resolve the UTXO being spent.
    let txout = if let Some(ref utxo) = psbt_input.witness_utxo {
        utxo
    } else if let Some(ref tx) = psbt_input.non_witness_utxo {
        tx.output
            .get(input.previous_output.vout as usize)
            .ok_or_else(|| {
                HWIError::Device(format!(
                    "non_witness_utxo missing output {}",
                    input.previous_output.vout
                ))
            })?
    } else {
        return Err(HWIError::Device(format!(
            "PSBT input {} has no UTXO",
            input_index
        )));
    };

    // For Taproot outputs the derivation is in tap_key_origins; for everything else bip32_derivation.
    let (address_n, script_type) = if txout.script_pubkey.is_p2tr() {
        let path = psbt_input
            .tap_key_origins
            .values()
            .find_map(|(_, key_source)| {
                if key_source.0 == trezor_fp {
                    Some(&key_source.1)
                } else {
                    None
                }
            })
            .ok_or_else(|| {
                HWIError::Device(
                    "Trezor key not found in PSBT tap_key_origins for this input".to_string(),
                )
            })?;
        let address_n: Vec<u32> = path.into_iter().map(|i| u32::from(*i)).collect();
        (address_n, InputScriptType::SPENDTAPROOT)
    } else {
        let path = psbt_input
            .bip32_derivation
            .values()
            .find_map(|(fp, path)| if *fp == trezor_fp { Some(path) } else { None })
            .ok_or_else(|| {
                HWIError::Device(
                    "Trezor key not found in PSBT bip32_derivation for this input".to_string(),
                )
            })?;
        let address_n: Vec<u32> = path.into_iter().map(|i| u32::from(*i)).collect();
        let script_type = if txout.script_pubkey.is_p2pkh() {
            InputScriptType::SPENDADDRESS
        } else if txout.script_pubkey.is_p2wpkh() || txout.script_pubkey.is_p2wsh() {
            InputScriptType::SPENDWITNESS
        } else if txout.script_pubkey.is_p2sh() {
            InputScriptType::SPENDP2SHWITNESS
        } else {
            InputScriptType::EXTERNAL
        };
        (address_n, script_type)
    };

    // Reverse txid bytes (Trezor expects reversed byte order).
    let prev_hash =
        trezor_client::utils::to_rev_bytes(input.previous_output.txid.as_raw_hash()).to_vec();

    let mut data_input = TxInputType::new();
    data_input.address_n = address_n;
    data_input.set_prev_hash(prev_hash);
    data_input.set_prev_index(input.previous_output.vout);
    data_input.set_script_sig(input.script_sig.to_bytes());
    data_input.set_sequence(input.sequence.to_consensus_u32());
    data_input.set_script_type(script_type);
    data_input.set_amount(txout.value.to_sat());
    // For Taproot inputs, provide the actual UTXO scriptPubKey so the firmware
    // uses it directly for the BIP341 sighash computation rather than deriving
    // a key-path P2TR address from address_n (which would not match the NUMS
    // Taproot output used by coincube multisig wallets).
    if txout.script_pubkey.is_p2tr() {
        data_input.set_script_pubkey(txout.script_pubkey.to_bytes());
    }

    let mut txdata = TransactionType::new();
    txdata.inputs.push(data_input);
    let mut msg = TxAck::new();
    msg.tx = protobuf::MessageField::some(txdata);
    Ok(msg)
}
