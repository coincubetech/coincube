# COINCUBE | Tenshu desktop — feature-coverage inventory (design review)

- Source: `/Users/macstudio/git/coincubetech/coincube/coincube-gui/src` at baseline `8495c3c0` (read-only; nothing modified).
- Method: menu/tab enums (`app/menu.rs`, `gui/tab.rs`), installer step lists (`installer/mod.rs` L690–880), feature gates (`feature_flags.rs`, `app/features.rs`), plus string-literal extraction from every `app/view/**`, `installer/**`, pre-app and root-level flow file (test modules excluded). Line refs are `file:L<n>` at the baseline.
- Quoting: strings are verbatim. `{}` / `{name}` = runtime interpolation, `\u{2014}` = em dash as written in source. "…" inside a quote = truncated here for length, not in source.
- Artboard mapping uses only the artboard names + parenthetical scopes supplied in the brief; the artboards themselves were not opened. Where only a name was given (e.g. Launcher-Catalog, Journey, P-Settings) the mapping is marked "(by name)".

## 0. Shell, gates and global vocabulary

**Tab states** (`gui/tab.rs` L675–682 titles): `Installer` "Installer", `Loader` "Loading...", `Home` "Home", `Login` "Login", `PinEntry` "Enter PIN", `PasskeyUnlock` "Unlock with passkey", `App` (Cube name), `DuressActive`; fallback title "COINCUBE".

**Primary rail** (`app/menu.rs` TopLevel): "Cube", "Spark", "Liquid", "Vault", "Marketplace". Nav header "Tenshu"; Lightning handle line "{}@coincube.io" (`nav/mod.rs`). When no Vault: rail item "Add Vault" (`nav/primary.rs` L246).

**Chains** (`chain.rs` L242): "Bitcoin", "Testnet", "Testnet4", "Signet", "Regtest", "Bitcoin Blake2b", "Bitcoin Blake2b Testnet4"; tickers "BTC" / "BTCB2".

**Build-time feature flags** (`feature_flags.rs`, all `.env`-driven consts):
| Const | Env | Default | Hides |
|---|---|---|---|
| `PASSKEY_ENABLED` | `COINCUBE_ENABLE_PASSKEY` | off | Passkey/PIN toggle on Create Cube; opening passkey Cubes |
| `PASSKEY_CREATION_AVAILABLE` | = above && macOS | off | Passkey creation |
| `CUBE_MEMBERS_UI_ENABLED` | `COINCUBE_CUBE_MEMBERS_UI` | off | Cube → Settings → Members rail item |
| `RECOVER_VAULT_ENABLED` | `COINCUBE_ENABLE_RECOVER_VAULT` | off | Launcher "Recover a Vault" (also needs sign-in, non-Blake2b) |
| `OWNER_KEYCHAIN_RECOVERY_ENABLED` | `COINCUBE_OWNER_KEYCHAIN_RECOVERY` | **on** (kill switch) | "Use my phone"/"Use both" kit modes; "Recover with phone" |

**Runtime gates** (`app/features.rs`): per-network — Spark: Bitcoin+Regtest; Liquid, Buy/Sell: Bitcoin only; P2P: Bitcoin, or test network with a test Mostro coordinator, never Blake2b. Unavailable reason template: "{} isn't available on {}." (greyed with hover popover). Server flags from `GET /connect/features` (fail closed, *hidden*): Marketplace master + Buy/Sell + P2P ("The Marketplace isn't available right now."); Liquid sunset gate = account `liquidEnabled` OR local Liquid state ("The Liquid wallet isn't available on this account."); Duress = `duressEnabled` OR enrolled, AND plan entitlement; Blake2b = account grant ("Bitcoin Blake2b isn't enabled for this account.") + Global Settings beta toggle. Liquid Swap: mainnet only.

---

## 1. Home launcher (`home.rs`, tab `Home`)

### 1.1 Cube list ("Your Cubes")
- (a) Tab Home → section Cubes (default).
- (b) Pick, open, rename, delete or restore Cubes for the selected network.
- (c) Heading "Your Cubes"; network pick_list top-right; row status lines: "Vault import pending", "Vault import interrupted — resume import", "Vault · Imported wallet · Sync incomplete", "Vault · Imported wallet", "No Vault configured", "Vault configured", "Vault {}"; Connect status: "Backed up to Connect — recovery kit ready", "Registered to Connect — Recovery Kit needs updating for imported wallet", "Registered to Connect — no recovery kit", "Not yet synced to Connect. It will sync automatically in a moment.", "Sovereign — local only". Remote-only Cube rows: "Recovery Kit + Phone recovery available — click to restore", "Recovery Kit available — click the download icon to restore", "Phone recovery available — click the download icon to restore", "Recovery Kit has no Master Seed — restore from the device where it was backed up", "Registered in Connect (no Recovery Kit backed up)"; tooltips "Recover this Cube — choose password or phone", "Restore this Cube from its Recovery Kit", "Recover this Cube with your phone". Rename modal: "Rename Cube", field "Cube Name", "Save". Plan cap: "Cube limit reached", "Cube limit reached ({}/{}) on the {} plan. Upgrade your Connect account to create more.", "…or delete a remote Cube to create one here."; post-create "Out of Cube slots — this Cube is saved on this device only.\n\nYou're using {}/{} Cubes on the {} plan for this network. …".
- (d) Error card at top (`self.error`); chain-unavailable screen = h3 network label + reason + "Back to Bitcoin"; config errors "There is an issue with the configuration for this network. You most likely use a descriptor containing one or more public key(s) without origin.", "This Cube's settings say it lives on {}, but it was found in the {} folder. Tenshu won't open it until the two agree."; signed-out vs signed-in (remote Cubes listed only when signed in).
- (e) Delete requires PIN; remote delete has warning (see 1.6).
- (f) Launcher-3D, Launcher-Iso, Launcher-Catalog (by name), V2-Launcher-3D, V2-Launcher-Faces, V3-Launcher-Spin.

### 1.2 Network dropdown + Global Settings
- (a) Home → pick_list (top-right) / sidebar "Settings" → Global Settings.
- (b) Switch chain; device-wide prefs.
- (c) "Global Settings", "Preferences apply to all Cubes on this device. Changes are saved automatically.", toggles "Developer mode" / "Show test networks in the Cubes network dropdown.", "Bitcoin Blake2b - Beta" / "Show Bitcoin Blake2b beta features in the app.". Errors "Couldn't save developer mode: {e}", "Couldn't save Bitcoin Blake2b beta: {e}".
- (d) Dropdown = ["Bitcoin"] normally; all test networks when Developer mode; "+ Bitcoin Blake2b" when Blake2b available (+ Testnet4 variant in dev mode) (`home.rs` L1142–1155).
- (e) none.
- (f) V3-Launcher-AppPrefs (by name); Launcher-States (by name) for network states — partial.

### 1.3 Launcher sidebar
- (c) "Cubes"; "Connect" group: "Overview", "Contacts", "Plan & Billing", "Security", "Duress" (gated by Duress gate); "Recover a Vault" (flag + signed in + not Blake2b); "Split a Bitcoin wallet" (network Bitcoin + Blake2b available via Connect); "Settings"; "Sign In".
- (f) V3-Launcher-Connect (Connect items); V3-Launcher-Spin.

### 1.4 Create Cube form
- (a) Home → "Create Cube" (or the default screen when no Cube; "Back to Cube list" when a list exists).
- (b) Name + unlock method → creates the Cube (Spark/Liquid seed).
- (c) Heading "Create a new Cube" (or "Create a Cube for your Bitcoin wallet" in import mode, with "Create and back up your Cube first, then import the public descriptors for your existing Bitcoin wallet. Your original wallet signers remain necessary to spend."); intro "A Cube is your account which can contain a Spark wallet, a Liquid wallet, a Vault wallet and other features."; signed-out note "You're signed out. This Cube will be saved on this device only. Sign in to Connect to sync it to your other devices and the Keychain mobile app."; "Unlock method:" "Passkey" / "PIN"; name validation "Please enter a name"; "Enter PIN:", "Confirm PIN:"; button "Creating" while busy. Passkey copy: "Your Cube will be secured with a passkey. No PIN is needed \u{2014} you'll use Touch ID to unlock it." (variants "Windows Hello", "a FIDO2 security key"); "Your backup is your Recovery Kit. Create one in Settings \u{2014} it carries this Cube's master seed, encrypted with a recovery password only you know, so you can restore the Cube even without your passkey.". Errors "Please enter all 4 PIN digits", "Please confirm all 4 PIN digits", "PIN codes do not match", "Failed to create Cube: {}", "Cube limit reached ({}/{}) for the {} plan. Upgrade your Connect account to create more Cubes.".
- (d) Passkey toggle hidden unless `PASSKEY_CREATION_AVAILABLE`; Blake2b network: "Create a Bitcoin Blake2b Vault. Other wallet and recovery flows are unavailable." + "Create Vault".
- (e) PIN double entry; seed encrypted (Argon2id) — no PIN hash.
- (f) Journey (by name) — partial; V3-Launcher-Spin covers unlock not creation.

### 1.5 Creation backup gate (post-create)
- (a) Create Cube → backup step (`creation_backup_view`, `home.rs` L5832–6075).
- (b) Force a backup or an explicit acknowledgement before the Cube is written.
- (c) Choice: "Back up this Cube", "Your recovery phrase is twelve words that rebuild this Cube on any computer. …", "Write down my recovery phrase", "Save a Recovery Kit to Connect", "Sign in to COINCUBE Connect to save a Recovery Kit instead. Your written phrase works either way.", link "I'll do this later". NOT_A_BACKUP_COPY (`services/unlock/creation_gate.rs` L95): "Copying the Coincube folder is not a backup. Part of this Cube's encryption key is stored in this computer's system keychain and never leaves it, …" (passkey variant L121). Phrase/verify screens reuse 4.3. Kit password reuses 4.4. Upload: "Saving your Recovery Kit…", "Encrypting this Cube's master seed with your recovery password and saving it to COINCUBE Connect. Your Cube is created once this finishes.". Bypass: "Create this Cube without a backup?", checkbox "I understand that without a backup, losing this computer means losing the bitcoin in this Cube, and that nobody — including COINCUBE — can recover it.", "Back up now", "Create without a backup", "Cancel". Errors: "The words you entered don't match. Please try again.", "Password must be at least {} characters." (12), "Passwords don't match.", "Confirm you've written the recovery password down.", "You're signed out of Connect, so there's nowhere to save a Recovery Kit. Sign in and try again, or choose \"I'll do this later\".", "Connect didn't store this Cube's encrypted seed. Nothing was written — please try again.", "This Cube can't be created without a backup or an acknowledgement. Nothing was written.".
- (d) Kit option needs sign-in; saving state.
- (e) Word verification; acknowledgement checkbox; nothing written on failure.
- (f) none (Journey by name — partial at best).

### 1.6 Delete Cube / Delete Remote Cube / Recover-choice modals
- (c) "Delete Cube", "Are you sure you want to delete the Cube \"{}\" and {}?" ({} = "its associated Vault configuration" / "all its associated data (including Vault)"), "(The Tenshu-managed Bitcoin node for this network will not be affected by this action.)", checkbox "Also delete Connect backup (frees a Cube slot)" → "The Connect backup will be permanently deleted. This frees a Cube slot on your account.", "Enter your PIN to confirm:", "Incorrect PIN. Please try again.", success "Cube successfully deleted". Remote: "Delete Remote Cube \"{}\"", "This Cube exists on the Connect server but not on this device. Deleting it will permanently remove it and free a Cube slot.", "WARNING: This cannot be undone.", "If another device still has this Cube locally, it will re-sync …", "Delete Remote Cube", "Deleting...". Recover choice: "Recover \"{}\"", "This Cube has two backups. Choose how to recover it — both restore the same wallet.", "Recover with password", "Recover with phone".
- (e) PIN confirmation; explicit opt-in for server deletion.
- (f) none.

### 1.7 Passkey registration overlay
- (c) "Passkey Registration", "Complete the passkey setup in the window below.", "Authenticate with Touch ID to create your passkey.\nLook for the system prompt.", "Cancel"; errors "Failed to start passkey ceremony: {}", "Failed to open passkey webview. Check your system's WebView support.", "Unexpected authentication response during registration.".
- (d) `PASSKEY_CREATION_AVAILABLE`. (e) WebAuthn PRF; seed never on disk.
- (f) none (V3-Launcher-Spin covers passkey *unlock*).

### 1.8 Liquid mnemonic restore ("Recover Wallet")
- (a) Home → recover entry → `recovery_input_view` (`home.rs` L6076, L6207).
- (c) "Recover Wallet", "Enter Recovery Phrase", "Enter your 12-word recovery phrase to restore your Liquid wallet.", 12 inputs with placeholders "1." … "12."; errors "Failed to restore from mnemonic: {}", "Failed to store master seed mnemonic: {}".
- (f) none.

### 1.9 Import Bitcoin wallet (imported descriptors; `app/state/imported_bitcoin.rs`, `wallet_source_form.rs`)
- (a) Home (Bitcoin) → "Import Bitcoin wallet" → Create Cube (import mode) → review/import form.
- (b) Watch an external (Sparrow/Electrum/hardware) wallet in a Cube; signing stays external.
- (c) "Import a Bitcoin wallet" / "Review Bitcoin wallet", "Use your existing Bitcoin wallet in this Cube. Signing stays with your original wallet or hardware device."; source tabs "Public descriptor" / "Hardware wallet"; fields "External / receive descriptor" (placeholder "wpkh([fingerprint/path]xpub.../0/*)"), "Internal / change descriptor (optional)" ("wpkh([fingerprint/path]xpub.../1/*)"); "Where should the history scan start?" "From the beginning" / "From a creation date"; "Addresses to search per branch" ("Start with 200 receive and 200 change addresses. …"); "Bitcoin server": "COINCUBE Connect", "Public Esplora (mempool.space)", "My Esplora server", "My Electrum server", "Server URL"; "Review wallet"; summary "Wallet details", "Bitcoin mainnet · {script} · {threshold}", "Signer fingerprints: {}", "First receive address", "Copy address". Validation: "Enter the wallet creation date in YYYY-MM-DD format.", "Choose a date from January 3, 2009 through today, or scan from the beginning.", "Enter an initial discovery window between 1 and 10,000 addresses per branch.", "Enter an HTTPS Esplora URL, or a localhost HTTP URL.", "Enter an SSL Electrum URL, or a local TCP URL.", "Sign in to Connect or choose another Bitcoin server.".
- (d) Sync errors "Could not synchronize this Bitcoin wallet", "This Bitcoin server reported a different wallet"; buttons "Retry synchronization", "Refresh status"; send/receive blocked until "Wait for the complete Bitcoin history scan before receiving." / "…sending.".
- (e) Unsigned PSBT round-trip: "Save unsigned Bitcoin PSBT", "Choose a signed Bitcoin PSBT", "Paste a base64 Bitcoin PSBT.", "PSBT saved. Open it in the original signing wallet, then load the signed file here.", "Signed PSBT loaded and verified.", "More signer signatures are needed before broadcast."; conflicting-signature checks.
- (f) none.

### 1.10 Recover a Vault (heir discovery; `recover_vault.rs`)
- (a) Home sidebar "Recover a Vault" → list → installer `RecoverInheritedVault`.
- (c) "Recover a Vault", "Vaults you're a keyholder for appear here. When a vault's recovery window opens, you can recover it with your Keychain — …", "Loading recoverable vaults…", "No vaults are available for you to recover right now.", "Vault #{}", scope chips "Full Cube" / "Vault only" / "Assisted recovery", buttons "Recover full Cube" / "Recover Vault", badge "Recovery open", statuses "Recovery is open — a keyholder of this vault can complete it.", "Recovery isn't open yet — expected around {}. We'll email you.", "Preparing recovery…", "Recovery data received — approve on your Keychain"; signed-out "Sign in to your account to see vaults you can recover.".
- (d) `RECOVER_VAULT_ENABLED` + signed in. (e) Keychain relay-decrypt (ECIES).
- (f) V3-Launcher-Connect ("recovery services") — partial.

### 1.11 Split a Bitcoin wallet (`split_wallet.rs`)
- (a) Home sidebar "Split a Bitcoin wallet".
- (b) Scan a non-Cube Bitcoin wallet for pre-fork BTCB2 and route it into a Blake2b Cube.
- (c) "Split a Bitcoin wallet", "Find BTCB2 held by a Sparrow, Electrum, Coldcard or other non-Cube Bitcoin wallet. …", "Choose a Bitcoin Blake2b Cube", "Original Bitcoin wallet", "Receive and change addresses 0–{}", "Paired Bitcoin Blake2b Cube", "Destination Cube", "Ready to scan. An incomplete scan is always an error, never a zero balance.", "Check BTCB2 availability", "Scan wallet", "Scanning…", results "Found {} UTXO{} ({} confirmed), totaling {} sats", "Pre-fork (splittable): …", "No spendable outputs were found in this bounded scan.", "Taproot (tr) wallets can be scanned but not split: there is no signing route for them yet.", "Continue to destination Cube"; gates "Sign in to Connect before scanning a Bitcoin wallet.", "Create a Bitcoin Blake2b Vault before scanning a Bitcoin wallet.".
- (e) "No balance conclusion was made." on every incomplete/unauthenticated scan.
- (f) V3-Vault-Split — partial (launcher entry + scan not covered as far as names show).

---

## 2. Installer (tab `Installer`; `installer/mod.rs`, `installer/step/*`, `installer/view/*`)

Network banner on every step (`network_banner`). Step sequences (`installer/mod.rs` L690–880):
- **CreateWallet (Vault)**: DefineDescriptor → BackupMnemonic → RegisterDescriptor → CoincubeConnect → SelectBitcoindType → InternalBitcoind → DefineNode(Esplora) → Final.
- **CreateWallet on Blake2b**: RestorePinSetup → DefineDescriptor → BackupMnemonic → RegisterDescriptor → CoincubeConnect → SelectBitcoindType → InternalBitcoind → WalletAlias → Final.
- **AddWallet**: ChooseBackend → RemoteBackendLogin → ImportRemoteWallet → ImportDescriptor → RecoveryKitRestore → RegisterDescriptor → CoincubeConnect → SelectBitcoindType → InternalBitcoind → Final.
- **RestoreFromRecoveryKit**: RecoveryKitRestore(Full) → RestorePinSetup → CoincubeConnect → SelectBitcoindType → InternalBitcoind → DefineNode(Esplora) → Final.
- **RestoreVaultFromRecoveryKit** (from running Cube "Restore from Connect"): RecoveryKitRestore(descriptor) → RegisterDescriptor → SelectBitcoindType → InternalBitcoind → Final.
- **RecoverInheritedVault** full: InheritanceRestore → RestorePinSetup → CoincubeConnect → SelectBitcoindType → InternalBitcoind → DefineNode → Final; descriptor-only: InheritanceRestore → RegisterDescriptor → SelectBitcoindType → InternalBitcoind → Final.
- **RecoverOwnCubeWithPhone**: same two shapes with OwnerKeychainRestore.
- **ClaimBlake2b**: ClaimOverview → RegisterDescriptor(import) → CoincubeConnect → SelectBitcoindType → InternalBitcoind → WalletAlias → Final.

### 2.1 Choose wallet type (template picker)
- (c) "Choose wallet type"; cards "Simple inheritance" ("Two keys required, one for yourself to spend and another for your heir."), "Expanding multisig" ("Two keys required to spend, with an extra key as a backup."), "2-of-3 multisig with inheritance" ("Three keys, any two spend, plus an inheritance key for your heir."), "Multisig with inheritance & backup recovery" ("A 2-of-3 spending policy with an inheritance path and a second backup recovery path."), "Expanding multisig with inheritance & backup recovery" ("Like the above, but your primary keys also help recover via a 2-of-6 inheritance path, …"), "Build your own" ("Create a custom setup that fits all your needs.").
- (f) none.

### 2.2 Template intro + diagram
- (c) "Introduction"; e.g. "Simple inheritance wallet", "For this setup you will need 2 Keys: Your Primary Key (for yourself) and an Inheritance Key (for your heir). …", "Primary spending policy:", "Recovery spending policy:", "Inheritance spending policy:", "Second recovery policy:", "2-of-2 multisig", "2-of-3 multisig", "2-of-6 multisig"; Keychain tips "Tip: instead of coordinating a hardware wallet for your heir, you can invite a family member or friend to contribute a Keychain key …"; diagram "CAN SPEND", "CAN'T SPEND (TIMELOCKED)", "Receipt of funds", "After some time* of wallet inactivity", "*The time range (timelock) for the activation of keys can be configured in the next step."; "Next".
- (f) none.

### 2.3 Descriptor editor ("Set keys")
- (c) "Set keys", "Advanced settings", "Primary spending option:", "Primary key #{}", "Recovery option #{}:", "Inheritance recovery option:", "Inheritance key #{}", "Add recovery option", "Add Safety Net", "Safety Net:", "Clear All", "Customize", "Continue"; descriptor type "Taproot" (Blake2b: "Bitcoin Blake2b Vaults use P2WSH"; "Taproot requires Bitcoin Core or Knots 26.0 or above"), "Descriptor type", "Threshold:", "Keys can move the funds after inactivity of:" (placeholder "ex: 1000"; "Value must be superior to 0 and inferior to 65535"), "Edit", "Set", "Apply", "Add key", "Add Safety Net key". Policy errors (`installer/descriptor.rs`): "No two recovery options may become available at the very same date.", "A path cannot contain only cosigner keys.", "No signer on this path can make replay-protected signatures: …".
- (f) V3-Vault-Advanced ("descriptors & keys") — partial (post-creation view, not the builder).

### 2.4 Select key source (key modal; `editor/key.rs`)
- (c) "Select key source"; sources "Cube Key" ("A key generated on this device, stored either encrypted with your Cube PIN or derived from a Passkey."; "Your Cube Key is already placed in this vault."), "Hardware Device" ("Use a plugged-in hardware signer. Supported: Ledger Nano S/S+/X, Coldcard Mk3/Mk4/Q, Jade, BitBox02, Trezor, Specter-DIY."), "Border Wallet", "Import xpub" / "Import xpub File", "Paste xpub" (placeholder "xpub…", "Paste", "Paste an extended public key (xpub) to add it as a signer."), "Keychain Keys" → "My Keychain Keys" ("Fetching Keychain keys…", "No Keychain keys. Add one in the COINCUBE mobile app.", "Failed to load keys: {}", "Retry") and "Contact Keychain Keys" ("Fetching contact keys…", "None of your contacts have shared keys yet.", "Contact"); per-key badges "Recovery key — restores this Cube, never signs", "Used by another Vault", "Already used in this Vault", "Already selected"; "Sign in to Connect to use Keychain keys."; hardware list "Detected hardware", "- No other sources detected -", "Already used sources", "Plug in a hardware device ...", "Pairing code: {code}", "Please unlock the device", "Please open the app on device", "This device does not support taproot", "Device version not supported, upgrade to version > {minimal_supported_version}"; "Enter a Safety Net token", "Enter a Cosigner token"; alias "Key name (alias):", "Give this key a friendly name. It will help you identify it later:", placeholder "E.g. My Hardware Wallet", "Key path account:", "Switch account if you already uses the same hardware in other configurations", "Replace", "Apply"; errors "Wrong derivation path", "Wrong network", "Key already used", "Origin missing", "Invalid Xpub", "Duplicate token", "Invalid token!", "This alias is already used for another key", "This Keychain key is already used elsewhere in this Vault.", "This key source is not available for Bitcoin Blake2b yet".
- (e) Hardware firmware advisories (`hw_advisory.rs`): "Firmware advisory", "Coldcard firmware advisory" / "Read the rotation guide", "BitBox02 firmware advisory" / "Read the update guide".
- (f) V3-Vault-Advanced — partial (keys listed, not the picker).

### 2.5 Border Wallet wizard (`border_wallet_wizard.rs`)
- (c) "Border Wallet", "What is a Border Wallet?", 4-step explainer, "Get Started"; "Recovery Phrase" ("Generate Random Phrase", "Derive from Master Key", "Word {}"); "Select Pattern" ("Select 11 cells from the grid in order. This pattern is your key \u{2014} remember it exactly.", "Selected: {} / {} cells", "Undo", "Clear"); "Checksum & Key" ("Derived Key Info", "Fingerprint: {}", "Derivation Path: {}", "Checksum word: \"{}\""); "Important \u{2014} Read carefully" ("After confirming, the mnemonic and private keys will be permanently erased from memory. …"), "Confirm & Enroll Key". Errors "Invalid recovery phrase. Please enter a valid 12-word BIP39 mnemonic.", "Please select exactly {} cells. Currently selected: {}".
- (e) Key erased after enrol; three-component warning. (f) none.

### 2.6 Backup Cube master seed (BackupMnemonic)
- (c) "Back Up Your Cube Master Seed", "Write these words down in order and keep them private. They recover this Cube; the PIN only unlocks this device.", checkbox "I have written down my seed words"; "Reminder: Back Up Your Master Seed"; prompt `installer/prompt.rs` L13 "This Vault key is derived from your Master Key. …".
- (f) Journey (by name) — partial.

### 2.7 Register descriptor / Back up descriptor
- (c) "Register descriptor", "This step is only necessary if you are using a signing device.", "Select hardware wallet to register descriptor on:", "I have registered the descriptor on my device(s)", "Failed to register descriptor"; "Back Up your wallet configuration (Descriptor)", "Back Up Descriptor", "Learn more", "I have backed up my descriptor", "The descriptor:", "Copy", "The wallet policy:", "{} blocks (~{})", "(Safety Net path)", "(Recovery path #{})".
- (e) Hardware registration; descriptor backup acknowledgement. (f) none.

### 2.8 Coincube Connect step (sign-in during install)
- (c) Header "COINCUBE | Connect"; toggle "Create Account" / "Sign In", "Already have an account? Sign in", "New to COINCUBE | Connect? Create account"; "Create a free COINCUBE | Connect account to unlock:" "  •  Hosted Esplora server — use your Vault instantly while your node syncs", "  •  Encrypted wallet descriptor backup", "  •  Integration with the companion Keychain mobile app"; "Enter your email address to receive a verification code." placeholder "your@email.com", "Please enter a valid email address"; "Enter the verification code sent to {}" placeholder "6-digit code", "Invalid verification code", "Resend Code", "Skip for now"; "Email not verified".
- (d) Blake2b: "Connect authentication is required for Bitcoin Blake2b" (no skip). (f) V3-Launcher-Connect — partial (launcher sign-in, not installer step).

### 2.9 Backend / node choice (SelectBitcoindType, InternalBitcoind, DefineNode)
- (c) Choice "Use COINCUBE | Connect" / "Start with COINCUBE | Connect" ("Your Vault will use our hosted Esplora server immediately (backed by Bitcoin Knots). No setup required."), checkbox "Also install a pruned Bitcoin node on my device", "Install a local pruned node", "I already have a node" ("Select this option if you already have a Bitcoin node running locally or remotely. Tenshu will connect to it."), "Install a node locally" ("Tenshu will install a pruned node on your computer. No local node required beforehand."), "Install a node only", "COINCUBE | Connect only", "▸  Advanced options"; node software "Node software": "Bitcoin Knots — stricter relay defaults; … Recommended.", "Bitcoin Core — the reference implementation, with its default relay policy.", "Bitcoin Knots (Bitcoin Blake2b) — …"; resources "Node data to keep" ("Minimal", "Compact", "Default", "Custom (MB):"), "Mempool memory cap" ("Small (100 MB)", "Default (300 MB)", "Leave blank for the default (300 MB)."), presets "Small computer" / "Regular computer", "Estimated total: about {} GB of disk, and up to {} MB of memory for the mempool."; validation "Enter a prune target of at least 550 MB, and a mempool cap of at least 5 MB …". Install progress "Download & start", "Downloading {flavor_name} {version}... {progress:.2}%", "Download complete", "Installing bitcoind...", "Installation complete", "Tenshu-managed bitcoind already installed", "Starting...", "Started". Own node "Set up connection to the Bitcoin node", "Node type:" "Bitcoin Core" / "Electrum" / "Esplora", "Address:", "RPC authentication:" "Cookie path" / "User" / "Password", "Esplora URL:", "Check connection", "Checking connection...", "Connection checked", "Connection failed: {detail}"; errors "Please enter correct address", "Please enter correct address (including port), optionally prefixed with tcp:// or ssl://", "Please enter a valid URL starting with http:// or https://", "Invalid Esplora URL", "Esplora URL is not for {} (height {}, genesis {})". Blake2b: "Bitcoin Knots for Bitcoin Blake2b", "Syncing your local Bitcoin Blake2b node", "Keep Tenshu open while the node syncs. This can take several hours. …".
- (e) Download verification: "Hashes do not match.", "Release SHA256SUMS signature did not verify against the pinned Knots signing key." (PGP-pinned).
- (f) none.

### 2.10 Recovery Kit restore (RecoveryKitRestoreStep)
- (c) "Restore from Cube Recovery Kit" / "Restore Wallet Descriptor from Connect", "Sign in to your Connect account to fetch your Cube Recovery Kit.", placeholder "you@example.com", "Email is invalid", "Send code", "Enter the 6-digit code we sent to your email.", "Invalid code", "Loading your Cubes from Connect…", "No Cubes found on this Connect account for the current network.", picker rows "{} — kit available" / "{} — no kit (disabled)" / "{} — no seed in kit (disabled)" / "{} — no descriptor in kit (disabled)", "Enter the recovery password for \"{}\".", "Recovery password", "Restore", "Decrypting your Recovery Kit. This takes a moment — Argon2id key derivation is intentionally slow.", "Ready to restore from \"{}\". Click Next to continue.", "Try again", "Skip — restore later"; errors "This Recovery Kit contains data for a different Cube or network. …", "Recovery kit download is delayed on new devices. Available at {}.".
- (e) Argon2id-encrypted kit; new-device download delay. (f) V3-Launcher-Connect ("recovery services") — partial.

### 2.11 Owner keychain restore ("with my phone")
- (c) "Recovering this Cube — approve the decryption on your phone when prompted…", "Sign in to your account to recover this Cube.", "Recovery is unavailable right now. Try again later.", "This phone backup belongs to a different Cube or chain.", "This seed backup needs a newer version of Tenshu.".
- (d) `OWNER_KEYCHAIN_RECOVERY_ENABLED`. (e) Phone approval of ECIES decrypt. (f) V3-Launcher-Connect — partial.

### 2.12 Inheritance restore (heir)
- (c) "Recovering this Vault — approve the decryption on your Keychain when prompted…", "There's nothing escrowed for you to recover here.", "Sign in to your account to recover this Vault.", "This Vault isn't set up for the kind of recovery you started.".
- (d) `RECOVER_VAULT_ENABLED`. (f) none.

### 2.13 Import descriptor / shared wallet / backup (AddWallet)
- (c) "Load a previously used wallet", "You have no current wallets", "Load a shared wallet", "Type the invitation token you received by email", "Accept invitation for wallet:", "Accept", "Invitation token is invalid or expired", "Load a wallet from descriptor", "Descriptor:", "Either descriptor is invalid or incompatible with network", "Import descriptor", "Import backup", "Backup successfully imported!", "Import the wallet", "Backup network do not match the selected network!"; Remote login "Choose backend", "Use your own node", "Login", "Choose an account you are already using:", "Send token", "An authentication token has been emailed to you", "Token", "Token is not valid", "Change Email", "Resend token". Decrypt .bed modal (`installer/decrypt.rs`): "Decrypt backup file", "Plug in and unlock a hardware device belonging to this setup to \nautomatically decrypt the backup", "Other options", "Upload extended public key file", "Paste an extended public key", "UNSAFE: Enter mnemonic of one of the keys" + " This option is not secure. I understand that entering a mnemonic on a computer may result in theft of my funds.".
- (e) Unsafe-mnemonic acknowledgement. (f) none.

### 2.14 PIN setup (RestorePinSetupStep) / Wallet alias / Final
- (c) "Create a PIN to secure your Cube", "Your Cube's mnemonic will be encrypted with this PIN on disk. …", "Enter a 4-digit PIN:", "Confirm PIN:", "PINs do not match."; "Give your wallet an alias", "Wallet alias:", "Wallet alias is too long.", "You will be able to change it later in Settings > Wallet" (default "My Vault {} wallet"); "Finalize installation", "Installing...", "Installed"; success line "Connect vault attached — approx. {}-day timelock ({} signer{}). …"; errors "This key (#{}) was already used in another Vault. …", "Connect vault sync failed: {}. Your local wallet is installed; the Connect vault can be retried later.".
- (f) V3-Launcher-Spin (PIN) — partial; alias/final none.

### 2.15 Claim overview (Blake2b claim target)
- (c) "Claim Bitcoin Blake2b", "Separate the coins in your Bitcoin Vault from their Bitcoin Blake2b counterparts. …", "Prepare your Blake2b Cube", "1. Split on Bitcoin", "2. Claim on Bitcoin Blake2b", "Both transactions require network fees. …", "Set up Blake2b Cube"; final "Creating the matching Blake2b Vault…", "Setup does not submit a claim transaction. …".
- (d) Blake2b grant + beta; passkey Cubes refused ("…Claiming from a passkey Cube isn't supported yet."). (f) V3-Vault-Split — partial.

---

## 3. Pre-app screens

### 3.1 Loader (`loader.rs`, tab `Loader`)
- (b) Start/connect daemon and sync before the App opens.
- (c) Stages: "Starting daemon...", "Scanning the blockchain..." + "Performing an initial scan of the Bitcoin blockchain via Esplora. This typically takes 1–2 minutes on the first run.", "Connecting to daemon...", "Syncing blockchain..." → "Almost there..." when progress > 0.98, plus live bitcoind log line. Error headline "Tenshu failed to start, please check if bitcoind is running" / "Tenshu failed to start: the Esplora chain backend is unreachable" / "…Electrum…" / "Tenshu failed to start"; warning banner "Error while starting Tenshu"; buttons "Back", "Retry"; "Connect startup can be retried in {} seconds. Wait for the backend to be ready, then Retry."; detail prefixes "Config error: {}", "Wallet error: {}", "Tenshu daemon error: {}", "Bitcoind error: {}".
- (d) Progress indicator = `loading::loading_indicator` (spinner) — no percentage shown in text. Spark: "Spark unavailable". Gated errors: "Sign in to Connect from Home to open this imported Bitcoin wallet", "Bitcoin Blake2b requires its isolated managed node".
- **Quotes section** (`coincube-ui/src/component/quote_display.rs`, data `coincube-ui/static/loading-quotes.json`, 65 quotes): one random quote per context ("loading", "syncing", "error"; also used elsewhere: first-launch, idle, empty-wallet, transaction-*, backup-reminder, balance-update; short quotes preferred for brief states; recency window avoids repeats), with an image (cube-active for loading/syncing, kage-bust for error). Attribution format: "— {author}, {source}"; scripture "— {reference}". Authors/sources: Jeff Booth ×4 (The Greatest Game, The Bitcoin Times Ed. 3, 2020); Saifedean Ammous ×2 (The Bitcoin Standard, 2018); Aleks Svetski ×4 (The Bushido of Bitcoin, 2024, pp. 56/297/465/466); Nick Szabo ×2 (Trusted Third Parties Are Security Holes, 2001; Shelling Out, 2002); Hal Finney ×3 (First tweet about Bitcoin, January 10, 2009; BitcoinTalk forum, 2010; Cypherpunks mailing list, November 1992); Satoshi Nakamoto ×12 (P2P Foundation forum post, 2009; BitcoinTalk forum, 2010; Cryptography mailing list, 2008; Bitcoin genesis block coinbase, 2009); James Dale Davidson & Lord William Rees-Mogg ×4 (The Sovereign Individual, 1997); R. Buckminster Fuller ×1 (Widely attributed; cited by Jeff Booth in The Greatest Game, 2020); Friedrich A. Hayek ×6 (Choice in Currency, 1976; Interview with James U. Blanchard III, 1984; The Road to Serfdom, 1944; The Fatal Conceit, 1988); Ludwig von Mises ×6 (The Theory of Money and Credit, 1912; Human Action, 1949; The Anti-Capitalistic Mentality, 1956; Inflation Must End in a Slump, 1951; Economic Policy, 1959); Murray N. Rothbard ×3 (What Has Government Done to Our Money?, 1963; The Case Against the Fed, 1994; The Mystery of Banking, 1983); Hans-Hermann Hoppe ×2 (Democracy: The God That Failed, 2001); Scripture (NKJV) ×16: Proverbs 20:10, 20:23, 11:1, 21:6, 13:22, 22:3, 21:20, 13:11, 10:4, 28:20; Leviticus 19:36; Deuteronomy 25:13, 15; Micah 6:11; Isaiah 1:22; Matthew 6:33; Ecclesiastes 5:10. (Full quote text lives in the JSON; each entry has an id such as `booth-01`, a `length` short/medium and a `contexts` list.)
- (f) V3-Launcher-Spin ("checks") — partial; Launcher-States (by name) — partial. Quotes: none known.

### 3.2 PinEntry (`pin_entry.rs`)
- (c) "Enter PIN for {}", 4-digit input (show/hide toggle), "Submit", "Back", "Loading your Cube..."; errors "Please enter all 4 digits", "Incorrect PIN. Please try again.", "PIN check failed to run: {e}", "Sign in to Connect before unlocking this Bitcoin Blake2b Cube".
- (e) Trial-decrypt (Argon2id); duress PIN silently triggers wipe (timing-matched). (f) V3-Launcher-Spin.

### 3.3 PasskeyUnlock (`passkey_unlock.rs`)
- (c) "Unlock {}", "Confirm with Touch ID to unlock this Cube.", "Unlock with passkey", "Back", "Loading your Cube...".
- (d) `PASSKEY_ENABLED` (else tab refuses to open). (e) PRF assertion; credential-to-Cube binding ("the passkey that answered belongs to a different Cube"). (f) V3-Launcher-Spin.

### 3.4 Login (remote backend; `services/connect/login.rs`, tab `Login`)
- (c) Header "COINCUBE | Connect", "Login", "An authentication was sent to your email:", "Token", "Token is not valid", "Resend token", "Connection failed", "Go back", "This wallet was deleted by its creator for all participants and cannot be opened. …".
- (f) V3-Launcher-Connect (sign-in email+code) — partial.

### 3.5 DuressActive (`app/view/duress/active_screen.rs`)
- (c) "Duress Mode Activated", "Use the Tenshu app on a trusted device to manage your account.", "Checking…", "Sign in to Connect"; elsewhere "Duress mode is active. Try again later.".
- (d) Shown after duress wipe/lock. (f) none.

---

## 4. Cube menu (App tab → "Cube")

### 4.1 Cube Overview (`app/view/global_home.rs`)
- (a) Cube → Overview.
- (c) "Total Balance", "Wallets", wallet cards "Spark" / "Liquid" ("Liquid Network") / "Vault", per-card "Send", "Receive", "Balance unavailable", "{} wallet unavailable", "Pending deposit", "-{} USDt pending" / "+{} USDt pending"; no-Vault card "Create Vault" / "Restore from Connect"; imported "Wallet import pending" / "Finish import"; "Transfer". Backup nag (`app/view/mod.rs`): "Your master seed phrase is not backed up. Back it up to avoid losing access to your Cube." + "Back Up Now". Stable-balance warnings: "Stable Balance was paused: converting bitcoin to USDB failed {} times in a row ({reason}). …", "Spark Settings".
- (d) Liquid card hidden when not usable; Spark per-network. (f) Cube-Compact, V2-Cube-Overview, V2-Cube-Compact, V3-Cube-Overview.

### 4.2 Transfer between wallets (global_home)
- (c) "Transfer Between Wallets", "< Back", "Sending from", "Receiving Wallet", "Enter Amount", "Amount in BTC" / "Amount in sats", "Enter an amount between {} and {} {}", "Minimum transfer: {} {}", "Loading limits…", "Next"; "Confirm Transfer", "Receiving Address", "Transferring to {destination_label} wallet", "Amount:", "Feerate (sats/vbyte):", "Fees:", "Total:", "Sign Transaction", "Confirm & Broadcast"; "Transfer broadcast", "Your transfer has been broadcast on-chain. It will appear in your {destination} wallet once it confirms. …", "Done"; "No receiving address available. Please generate one first.".
- (e) Explicit sign then broadcast. (f) V3-Cube-Transfer.

### 4.3 Settings → General (`app/view/settings/general.rs`)
- (c) "General"; "Network:"; "Show direction badges on transactions:"; "Recipient identity checks" + "Check supported payment requests with Branta before sending. Strict mode does not share the plaintext destination or payment amount, …" + "Branta privacy information"; "Primary balance value:" "Fiat" / "Bitcoin"; "Bitcoin display unit:" ("BTC"/"Sats"); "Fiat price:" + "Fiat price data is provided by third-party services. Availability and accuracy are not guaranteed.", "Exchange rate source:", "Currency:".
- (f) Cube-Workspace; P-Settings (by name).

### 4.4 Settings → Recovery (master seed, Recovery Kit, Vault Recovery Alerts)
- (c) Seed: "Backup Master Seed Phrase" / "Write down your 12-word recovery phrase as a backup. …" / "Start Backup"; done state "Master Seed Phrase Backed Up", "View Again". Backup flow (`settings/backup.rs`): "Enter PIN", "Enter your Cube PIN to unlock and display your 12-word recovery phrase.", "Unlock", "Back up your wallet", warnings "Do not share these words with anyone.", checkbox "I UNDERSTAND THAT IF I LOSE THESE WORDS, MY FUNDS CANNOT BE RECOVERED", "Show My Recovery Phrase", "Your Recovery Phrase", "I've Written It Down", "Verify Your Recovery Phrase", "Verify", "Backup Complete", "Back to Settings"; passkey "Backup via Passkey" ("Confirm with Touch ID to show this Cube's recovery phrase. …").
  Kit (`settings/recovery_kit.rs`): "Cube Recovery Kit", "Back up your Cube Recovery Kit", "Create Recovery Kit", "Recovery Kit backed up", "Last updated {}.", "Update", "Finish backing up", "Remove", "Protected by:" "Password"/"Keychain", "Includes:" "Master Seed Phrase"/"Wallet Descriptor"; mode picker "Choose how to protect your Recovery Kit" — "Use a recovery password" / "Use my phone" / "Use both", "Checking for your phone key…", "Phone key found. …", "No phone key yet. Create a recovery key in your Keychain app …", "Check again"; password "Recovery password" (placeholder "Choose a password"), "Strength: {}", "Confirm recovery password" ("Re-enter password"), checkbox "I've written this password down somewhere I can find it", "Back up Recovery Kit"; "Encrypting and uploading…", "Sealing to your phone…"; "Confirm with Touch ID" (passkey Cube); remove "Remove your Recovery Kit?", "COINCUBE can't undo this. …", "Remove backup"; drift "Your Wallet Descriptor changed since your last backup — update both copies now.", "⚠ Your password descriptor is out of sync with your Vault.".
  Vault Recovery Alerts: "Vault Recovery Alerts", toggle "Recovery alerts", "Turn off recovery alerts? …", "Turn off", "Keyholders who'd be notified", "What keyholders can recover" — "Nothing" / "Vault only" / "Full Cube", "Enter your PIN to include this Cube's seed in the encrypted recovery kit.", plan gate "Recovery alerts aren't available on your current plan. …" + "View plans", "Create a Vault and register it with Connect to enable recovery alerts.".
- (d) Signed-out "Sign in to Connect to back up your Cube Recovery Kit."; plan gates (Estate for keyholder kit); `OWNER_KEYCHAIN_RECOVERY_ENABLED` for phone modes.
- (e) PIN/Touch ID re-auth, word verification, Argon2id client-side encryption, written-down acknowledgements, removal confirmation.
- (f) Cube-Workspace; V3-Launcher-Connect ("recovery services") — partial.

### 4.5 Settings → About / Stats
- (c) About: "About", "Version", "Tenshu v{}", "coincube-gui v{}", "Connect Device", "Device ID", "Account", "Stream", "Re-register this device", "Re-registering…", "Re-registered. New device ID: {}", "Re-registration failed: {}". Stats: "Download Stats", "Refresh", "TOTAL DOWNLOADS", "+{} downloads today", "of {} downloads", "{:.2}% of goal", "No data", "Retry".
- (f) Cube-Workspace.

### 4.6 Settings → Avatar (renders from Connect panel)
- (c) "Avatar", "Choose Your Traits", "Your selections, combined with a hash of your Lightning address, create your unique avatar.", traits "Gender" (Man/Woman), "Archetype" (Ronin/Samurai/Shogun), "Age Feel" (Young/Mature/Elder), "Demeanor" (Calm/Fierce/Mysterious), "Armor" (Light/Standard/Heavy), "Accent" (Sun/Splatter/Seal/Calligraphy), "Laser Eyes" (On/Off); "Generate Avatar", "Forging your identity…", "Generating your sumi-e avatar. This may take up to 30 seconds.", "Regenerations: Unlimited" / "Regenerations remaining: {}", "No Regenerations Remaining", "Regenerate Avatar", "Download PNG", "Retry Connection".
- (d) Needs sign-in + Lightning Address: "Claim a Lightning Address first", "Your Avatar is bound to your Lightning Address. Claim one to set up an Avatar.", "Claim Lightning Address" / "Set Lightning Address First".
- (f) Cube-Workspace — partial (by section count).

### 4.7 Settings → Members
- (c) "Members", "People who can view this Cube and contribute signing keys.", "Refresh", "Pending Invites", "No members yet", "Add Member", placeholder "email@example.com", "Invite by email. Existing contacts are added immediately; new emails get an invite and join on accept.", "Joined after Vault" + "This member can attach keys but won't be in the current Vault's signing quorum. …", "Joined {}", "Remove", "Revoke", "Can't remove this member" / "Wind down the Vault(s) first, then try again." / "Dismiss", expiry "Expires in {} days" etc.
- (d) `CUBE_MEMBERS_UI_ENABLED` (default off). (f) Cube-Workspace — partial.

---

## 5. Connect menu (Home tab → "Connect"; `app/view/connect/*`)

### 5.1 Sign in / Sign up / OTP
- (c) "Sign in to COINCUBE", "Email", "Continue", "Signing in…", "Don't have an account? Sign up"; "Create an Account", "Create a COINCUBE account to access Connect and Buy/Sell", "Promo or referral code (optional)", "Signing up…", "Back"; "Verify OTP", "We sent a code to {email}", "Enter your OTP", placeholder "6-digit code", "Resend OTP ({cooldown}s)" / "Resend OTP", "Verifying…". Generic prompt (`sign_in_prompt.rs`): "Sign in to Connect", "Sign in to your Connect account to {}.", "Sign In". Session end: "Connect authorization ended. …".
- (f) V3-Launcher-Connect.

### 5.2 Overview
- (c) "Account Overview", "Plan" ("Free" / Pro / Estate), "Access until" / "Renews", "{date} · {cycle}", "Contacts", "Cubes", "Sign Out"; banners "Renewal reminder" / "Your {} plan renews on {}. Renew now to keep your features." / "Renew", "Plan expired" / "Your plan expired on {} — renew to restore premium features." / "View plans" / "Dismiss"; "Tap to retry"; "Loading…".
- (f) V3-Launcher-Connect.

### 5.3 Plan & Billing (+ checkout, history, promo)
- (c) "Plan & Billing", "Monthly" / "Annual", "BEST VALUE", "${amount}", tier blurbs ("The essentials to create and recover your Cubes, with Esplora access and a Lightning address.", "More Cubes per network and Connect-enforced policies for greater control. Duress mode — Coming soon.", "Maximum Cube capacity, inheritance escrow, and collaborative multi-sig keyholder invitations."), "Current plan", "Upgrade to Pro" / "Upgrade to Estate" / "Upgrade", "Switch to Free/Pro/Estate", "Promotional access", "Expires {}", "Have a promo or referral code?", "Promo or referral code", "Redeem" / "Redeeming…", "Payments via Bitcoin (Lightning or on-chain). No recurring subscriptions — pay upfront, renewal reminders sent by email."; checkout "Checkout", "Creating invoice…", "Upgrade to {} ({})", "{} {} ({} sats)", "Copy", "On-chain: {}", "Open in Browser", "Expires: {}", "Payment detected, confirming…", "Payment confirmed! Upgraded to {}.", "Done", "Invoice expired.", "Try Again", "Cancel"; "View Billing History" → "Billing History", "No billing history yet.", statuses "Unpaid"/"Processing"/"Paid"/"Expired".
- (d) "Pricing temporarily unavailable.\nReconnect to the internet to view current plans and features.", "Purchasing is currently unavailable, so plans can't be bought right now. …", "A newer pricing update is available. …".
- (f) V3-Launcher-Connect (plan & billing).

### 5.4 Security
- (c) "Security", "Verified Devices" ("Loading devices…", "No verified devices on record.", "Unknown Device", " (this device)", "Last active: {}", "Added: {}", "Delete" / "Deleting..."), "Login Activity" ("Loading activity…", "No login activity on record.", "Successful login from {}", "Failed login attempt from {}").
- (f) V3-Launcher-Connect (devices & security).

### 5.5 Contacts
- (c) "Contacts", "Manage your trusted contacts.", "Received Invites", "Pending Invites", "No contacts yet", "Invite your first trusted contact to get started.", "Invite Contact", "Email Address" ("email@example.com"), "Also add to Cube(s) (optional)", "Send Invite" / "Sending…", "Resend", "Revoke", "Accept" / "Accepting…", expiry labels, contact detail "Connected {}", "Recovery Kit", "Associated Cubes", "Add to Cube(s)…", "No eligible cubes — …", "Add {} to the selected cubes.".
- (f) V3-Launcher-Connect (contacts).

### 5.6 Duress (status, enrolment, clear, emergency contacts)
- (c) Status: "Duress Mode", "What duress mode does", "Unlocking any one Cube with your duress PIN wipes every Cube on this device and signals Connect to lock your entire account — …", "Duress mode is enabled", "Disable Duress Mode", "Duress mode is enabled on your account" (other device); prerequisites "Set up a Cube Recovery Kit for each Cube", per-Cube "Backed up" / "Kit incomplete — Vault Wallet Descriptor missing" / "No recovery kit" + "Create Recovery Kit", "Finish the Recovery Kit for your Vault Cubes above to enable Duress Mode.", "Set up Duress Mode". Enrol wizard (`duress_enroll.rs`): "Set your duress PIN" ("Duress PIN (4 digits)", "Confirm duress PIN", "Must be exactly 4 digits.", "PINs do not match."), "Set your all-clear passphrase" (≥12 chars, "All-clear passphrase"), "Set your duress recovery-kit password" ("Duress recovery-kit password"), "Pick an unlock delay" (chips "24h" "48h" "7d" "30d" "90d"), "Memorize your credentials" + checkbox "I have memorized all credentials", "Next", "Back", "Complete enrollment" / "Enrolling…". Disable: "Confirm with a Cube unlock PIN" / "Unlock PIN for a PIN-protected Cube" / "Confirm with your passkey" / "Waiting for passkey…" / "Can't confirm on this device". Clear (locked account): "Account locked", "Account is locked until {when}. Come back then.", "Enter your all-clear passphrase", "Clear Duress", "Clearing…", "Forgot all-clear passphrase?", "Duress cleared. Download your Cube Recovery Kit to restore your Cubes.". Emergency contacts (`duress_contacts.rs`): "Emergency contacts", "Contacts ({}/{})", "Maximum reached", "+ Add contact", "No emergency contacts yet", "Add up to 5 people …", form "Name" ("e.g. Jane Doe"), "Phone (international format)", email ("jane@example.com"), "How should we reach them?" ("SMS" / "WhatsApp" / "Email"), "Save contact"; previews "What the alert says", "The one-time intro message"; plan gate "Available on the Estate plan".
- (d) Duress gate (server flag OR enrolled) AND Pro/Estate entitlement ("Duress mode is available on Pro and Estate plans. …"); "Couldn't verify your account status." + "Retry".
- (e) Recovery-kit prerequisite, memorise acknowledgement, real-PIN/passkey confirmation to disable, unlock delay.
- (f) none (V3-Launcher-Connect lists account/devices & security but not duress).

---

## 6. Marketplace (App → "Marketplace"; rail "Buy/Sell", P2P tertiary "Book", "My Trades", "Chat", "Create", "Settings")

### 6.1 Buy/Sell panel (`app/view/buysell/panel.rs`)
- (c) Header "| BUY/SELL"; own sign-in ("Sign in to your account", "Create a COINCUBE account to access Buy/Sell and other features", "Enter the OTP sent to your email", "Enter OTP code", "Resend in {}s"); location "Select Country: ", "Detecting your location...", "Please wait..."; mode "Buy Bitcoin using Fiat Currencies", "Sell Bitcoin to a Fiat Currency", "View Order History", "Log Out".
- (d) Bitcoin only; server flags (hidden). (f) none.

### 6.2 Mavapay (Africa: NGN/ZAR/KES)
- (c) "Powered by Mavapay", "Buy Bitcoin using Fiat Money" / "Sell Bitcoin to Fiat Money", "Input Transfer Amount (in Satoshis)", "Satoshis (BTCSAT)", "Generate Invoice", "Get Quote" / "Fetching Quote..", bank setup "Setup Bank Details (Nigeria)" / "(South Africa)" / "Setup Mobile Money Details (Kenya-MPESA)", "Enter Recipient Bank Account Number", "Select Recipient Bank", "Verify Details", "Is this the recipient's registered name?"; "Order Created Successfully", "Payment Instructions" (STEP 1–3, "Critical: Include this reference number"), "Important Notes" (incl. "Our commission (1-2%) will be deducted from the final Bitcoin amount"), "Purchase Complete" / "Withdrawal Complete", "Order History", "Transaction Summary", "Fulfil Order from Liquid Wallet", "Liquid Balance insufficient. Manually fulfill invoice", "Simulate Pay-In (Developer Option)"; statuses "Complete"/"Processing"/"Expired"/"Failed"; errors "Unable to create quote", "Unable to verify Bank Details", "Bank Account Number MUST be a number".
- (f) none.

### 6.3 Meld (rest of world, webview)
- (c) "Checking for regional support and transactional limits...", "Select Your Region" ("Search regions...", "No regions match your search"), "Input {} amount", "Minimum Transaction of {} is {}", "Maximum Transaction of {} is {}", "Input Amount of {} is greater than your BTC balance of {}", "Get Quotes", "Select a Deposit Address" ("Pick an existing address", "Generate New Address", "Load more"), "YOU SEND" / "YOU RECEIVE", "Provider", "Total Fee", "Exchange Rate", "Select a preferred provider", "Start Session", "Loading Webview...", "Your Recipient Address (Copy+Paste)", "Keyboard shortcuts are problematic in the webview for macOS. Right-click the input field to paste."; errors "Couldn't get a quote", "Couldn't open the payment page", "Your country: {} currently isn't supported for BuySell".
- (f) none.

### 6.4 P2P (Mostro) — Overview/Book, My Trades, Chat, Create Order, Settings (`app/view/p2p/*`)
- (c) Book: "P2P Order Book", "Browse and take P2P trading orders from the Mostro network", filters "BUY BTC ({})" / "SELL BTC ({})", "Search currency...", "PAYMENT METHODS", "Reputation Filter" ("Min rating:", "Min days active:"), order card "YOU ARE BUYING"/"YOU ARE SELLING", "Market Price", "Creator Reputation", "No rating", "Enter fiat amount ({} - {} {})", "Lightning invoice or address (optional)", "No orders found". Create: "Create P2P Order", "Order Type", "Fiat Amount", "Select the fiat currency you will pay with", "Market Rate"/"Fixed Price", "Sats Amount", premium ("e.g. 5 or -3", "Premium must be between -100 and 100"), "Select payment methods" + "Enter custom payment method"/"Add Custom" (presets e.g. "SEPA instant", "Interac e-Transfer", "Pago Móvil", "Cash App"), "Use registered address" / "Use different Lightning Address or invoice", "Please review your order:", "This order will be published for 24 hours.", "Cancel Order"; validation "Amount is required", "Trade size must be between {} and {} sats", "Node limits not loaded — order may be rejected". Trade: status headings ("Pay the hold invoice", "Send fiat payment", "Confirm Fiat Sent", "Buyer sent fiat", "Release Sats", "Submit your invoice", "Sats released", "Payment failed", "Cancellation requested", "Dispute opened", "Admin reviewing dispute", "Trade Complete!", "Rate your counterparty", "Submit Rating"); Spark hold-invoice pay "Payment Required", "Pay from Spark", "Spark balance: {} sats", "Lock amount: {} sats", "Network fee", "Confirm and pay", "Pay from another wallet", "Copy Invoice", "Trade canceled after Spark payment". My Trades: "My Trades", "Your active and completed P2P trades", "No trades yet", states "Locking Escrow", "Waiting Invoice", "Fiat Sent", "Paying Sats", "Payment Failed". Chat: "No conversations yet", "Dispute Chat", "Chat with Admin", "Type a message...", "Chat is read-only", "Send File", attachments "{} · Encrypted", 25 MB caps. Settings: "P2P Settings", "Mostro Nodes", "Select which Mostro node to connect to", "Add a new node" ("Node name", "Node pubkey (hex)", "Network:"), "Add Node", relays ("URL must start with wss:// or ws://", "Relay already exists"), "Test Relays"; default node "Mostro (Official)"/"Mostro (Default)".
- (d) Coordinator/network mismatch: "The active Mostro coordinator \"{}\" is a {} coordinator, but this wallet is on {}. Trading is disabled — …"; "Saved P2P config couldn't be loaded …"; relay errors "Failed to connect to any Mostro relay — check your internet connection", "Timeout waiting for Mostro response (30s) — …".
- (e) Release confirmation text "This action cannot be undone."; NIP-59 gift-wrap + ChaCha20 encrypted chat/attachments.
- (f) none.

---

## 7. Liquid (App → "Liquid": Overview, Send, Swap, Receive, Transactions, Settings)

- Gate: Liquid sunset gate + Bitcoin only; Swap mainnet only ("Not available on this network").
### 7.1 Overview — "{} USDt", "Last transactions", "No transactions yet", "Your transaction history will appear here once you send or receive coins.", "View All Transactions". (f) V3-Cube-Overview — partial; V3-Activity — partial.
### 7.2 Send (`liquid/send.rs`)
- (c) "YOU SEND"/"THEY RECEIVE", "Balance: {}" / "Balance: {} USDt", destination placeholders "Enter Lightning Invoice or Address", "Enter Bitcoin Address", "Enter Liquid Address", "Enter Liquid USDt Address", "Enter Ethereum USDt Address", "Enter Tron USDt Address", "Enter Binance Smart Chain USDt Address", "Enter Solana USDt Address"; "Amount ({})", "Amount (USDt)" ("e.g. 1.50"), "Pay fees with USDt"/"Pay fees with L-BTC", "Select Fiat Currency:", "Amount in {}", "Comment (Optional)", "Paying with {} → Receiver gets {} (swap)", confirm "Send amount:", "Fees (USDt):"/"Fees (L-BTC):", "Comment:", "Total:"; "Enter an amount between {} and {} {}".
- (e) Branta recipient check ("Recipient identified", "View with Branta") when enabled. (f) V3-Wallet-Send.
### 7.3 SideShift send (USDt to other chains) — "Paste USDt address (any network)…", "Select Network" ("This address is compatible with multiple networks. Please select one."), "Send to {}", "Amount to send (USDt)", "Fixed Rate"/"Variable Rate", "Review Swap", "Confirm & Send", "Swap Submitted", "Swap ID", "Recipient receives", "Service fee". (f) V3-Wallet-Send — partial.
### 7.4 Receive (`liquid/receive.rs` + SideShift receive)
- (c) Tabs Lightning/L-BTC/USDt/on-chain; "Share this invoice to receive sats via Lightning", "Share this address to receive L-BTC from any Liquid wallet", "Share this address to receive USDt (Liquid Tether) from any Liquid wallet", "Generate New Address", "Show QR Code", "Description (optional)", "Invoice details", "USDt amount (optional, e.g. 1.50)"; on-chain swap "Bitcoin onchain → L-BTC swap", "- Send between {} and {} — outside this range the swap cannot settle.", "- Use this address for ONE deposit only.", "- If your deposit is outside the range or the swap fails, funds are recoverable via Refund in the Transactions tab."; SideShift "Receive USDt", "Choose the network you are receiving from.", "Generate Deposit Address", "Only send {} USDt ({})", states "Waiting for deposit", "Deposit detected", "Settling…", "Settled ✓", "On hold for review", "Refunding…".
- (f) V3-Wallet-Receive.
### 7.5 Swap (L-BTC ↔ USDt via SideSwap)
- (c) "YOU PAY"/"YOU RECEIVE", "Swap All", "Balance:", "Review swap", "You pay"/"You receive", "SideSwap + network fee", "Quote expires in", "Quote expired.", "Confirm swap", "Swapping…", "Last swaps"; blocks "Wallet is still syncing — swaps are paused until it finishes.", "You need a little L-BTC to pay network fees for this swap. Receive some L-BTC first.".
- (d) Mainnet only. (f) none.
### 7.6 Transactions + refunds
- (c) Empty "Your Liquid wallet is ready. Once you send or receive\nfunds, they'll show up here." + "Send sats"/"Receive sats"; "Incoming payment"/"Outgoing payment", "USDt Transfer", "Paid in USDt", statuses "Timed Out", "Refund Pending", "Waiting Fee Acceptance"; "Refundable Transactions", "Refundable Swap", "This swap can be refunded", "Refund Details", "Bitcoin Address" ("Enter Bitcoin address for refund"), "Use Vault address", "Fee Rate (sat/vB)", "Refund...", "Refund broadcast · {}".
- (f) V3-Activity — partial (refund flow not named).
### 7.7 Settings — "Liquid Settings", "Seed phrase backup has moved to Cube Settings (General section).". (f) Cube-Workspace — partial.

---

## 8. Spark (App → "Spark": Overview, Send, Receive, Transactions, Settings → General / Lightning Address)

- Gate: Bitcoin + Regtest; bridge must be running ("Spark is not configured for this cube yet. Set up a Spark signer and restart the app to connect the bridge.", "Connecting to the Spark bridge…", "Spark bridge error").
### 8.1 Overview + last_tx — "Spark Wallet", "Last transactions", "View All Transactions", empty state as Liquid. (f) V3-Cube-Overview — partial.
### 8.2 Send (`spark/send.rs`)
- (c) "YOU SEND"/"THEY RECEIVE", "Amount (BTC)"/"Amount (sats)", "Amount in sats (optional for invoices with amount)", "This invoice sets its own amount.", "Replace payment request", "Enter a destination and amount above, then press Prepare to see the fee quote.", "Preparing send… asking the Spark bridge for a fee quote.", cross-chain "Stablecoin (cross-chain)", "Confirm the network", "{} on {} — via {}", "Max slippage, in basis points ({}–{}). …", "Get quote", "They receive", "Network fee", "Conversion fee", "Total you send", "Quote valid for {}s.", "This quote has expired. Get a fresh one to continue.", "Confirm and send", "Spark transfer — instant, lower fee than Lightning or on-chain.", "Send failed", "Payment status unconfirmed", "Checking your payment history to find out whether it went through. Don't send it again until this finishes.", "Check status", "Start over", "View transactions".
- (e) Address-verification guard: "This payment request’s address does not match its verification data. Do not send. Return and replace the payment request."; Branta check. (f) V3-Wallet-Send.
### 8.3 Receive (+ SideShift receive)
- (c) Lightning: "Amount in sats (optional)", "Description shown to the payer (optional)", "Invoice details", "Payment request", "Generate another", "QR code unavailable for this payload."; on-chain "Spark on-chain receive" (deposit-address model), "Pending deposits", "{} / {} confirmations", "Waiting for confirmations ({} required)", "Claiming…", "Claim failed: {}"; Spark address "Spark address" ("This is a static, reusable Spark address …"); "From another network" → SideShift "Refund address", "Generate deposit address", "Only send {} on {} ({})", "Send between {min} and {max} {} — amounts outside this range are refunded.", "Bitcoin arriving", "Bitcoin arrived", "Something went wrong" / "The swap couldn't be set up.".
- (f) V3-Wallet-Receive.
### 8.4 Transactions — "Loading payment history from the Spark bridge…", "Failed to load payment history", kinds "Lightning payment", "On-chain deposit", "On-chain withdrawal", "Spark transfer", "Stable Balance"; statuses "Timed out", "Refund pending", "Awaiting fee"; "Payment ID". (f) V3-Activity.
### 8.5 Settings → General — "Stable Balance" ("Keep a portion of your Bitcoin balance pegged to USD. …"), "Stable Balance is ON/OFF", "Turn on"/"Turn off", "Spark bridge unavailable — toggle disabled."; "Bridge status" ("Checking bridge…", "The Spark bridge subprocess is reachable and responding to JSON-RPC requests over stdio.", "The last get_info call failed. …"). (f) Cube-Workspace — partial (Stable Balance not named).
### 8.6 Settings → Lightning Address — "Claim Your Lightning Address", "Get a free Lightning Address to receive bitcoin from anyone.", "Choose a username for your Lightning Address", suffix "@coincube.io", "Checking…", "✓ Available", "This is your current username", "Claim Lightning Address" / "Claiming…", "Your Lightning Address", "Current: {}", "Anyone can send you bitcoin using this address. This app must be open and active to receive payments.", "Lightning address needs re-registration", change confirm "Change your Lightning Address?", "Your current address {} will stop working immediately.", "New address: {}", "Yes, change it". (e) Change confirmation. (f) Cube-Workspace.

---

## 9. Vault (App → "Vault": Overview, Send, Receive, Coins, Transactions, PSBTs, Recovery*, Claim BTCB2*, Settings)
(*Recovery hidden for imported wallets; "Claim BTCB2"/"Resume Claim" only with Blake2b grant + eligible Vault — `nav/vault.rs`.)

### 9.1 Overview (`vault/overview.rs`) + wallet header
- (c) Sync: "Syncing blockchain", "Loading Vault history", "Your local node is syncing — {:.1}% complete", "Balance unavailable: syncing has failed since {}{}. {}", "Last known balance: …", "Balance unknown until this Vault's history is scanned. Pick the date it was created to start." + "Pick a date"; recovery "≈ {} left before first recovery path becomes available.", "Recovery path is or will soon be available for {} coin(s).", "Refresh coins"; "Last transactions", "See more", "View All Transactions", tx row "Miner fee: ", "Date:", "Txid:", "See transaction details". Header (`wallet_header.rs`): "Checking for new transactions".
- (f) V3-Cube-Overview — partial; V3-Activity — partial.

### 9.2 Send / spend (`vault/spend/mod.rs`, `shared/feerate_picker.rs`)
- (c) "Self-transfer", "Batch label", "Add payment", per-recipient address ("Invalid address (maybe it is for another network?)"), "Payment label", "Amount BTC" ("0.001 (in BTC)") / "Amount (sats)" ("100 000 (in sats)"), "Enter amount in {}", max button tooltip "Total amount remaining after paying fee and any other recipients"; "Feerate:" "Fast (~10m)" / "Normal (~1h)" / "Slow (~4h)" / custom "42 (in sats/vbyte)"; "Coins selection"; errors "Please set a feerate.", "Invalid feerate.", "Duplicate recipient address.", "Two payment addresses are the same", "Please select at least one coin.", "Insufficient funds.", "Invalid amount. (Note amounts lower than 500 sats are invalid.)", "Feerate must be an integer less than or equal to 1000 sats/vbyte"; sync line "Syncing blockchain ({:.2}%)", "Wallet sync is failing; the balance may be out of date".
- (e) Recipient identity (Branta) where supported. (f) V3-Wallet-Send.

### 9.3 PSBT detail & signing (`vault/psbt.rs`, `vault/hw.rs`)
- (c) "Transaction is saved" / "Save this transaction", "Broadcast the transaction", "Broadcasting…", "Broadcast failed", "Delete this PSBT", "Successfully deleted this transaction.", "Go back to PSBTs", "Finalizing this transaction requires:", "Partially signed", "Not ready", "Remaining requirements", "{} more signature{}", "{} of {} signature{} collected", "Able to move the funds at any time.", "Available after inactivity of ~{}", signer kinds "This computer", "Hardware wallet", "Border Wallet", "Keychain · local network", "Sign with Connect", "Send through Connect", "Request from everyone", "Try Wi-Fi again", "Pair a phone", "Keychain signing needs Connect", "Reconstruct Border Wallet" (phrase + 11-cell pattern + checksum), "Insert updated PSBT:", "Please enter the correct base64 encoded PSBT", "Spend transaction is updated", "Sign or save the transaction first to enable export", "No external recipients — every output returns to this wallet.". Hardware: "The wallet descriptor is not registered on the device.\n You can register it in the settings.", "This signing device is not part of this spending path.".
- (e) Broadcast warnings "WARNING: Broadcasting this transaction will invalidate some pending payments." + dropped-tx list; Blake2b replay check "Could not check whether {} {} {} also on Bitcoin (Connect did not answer). You can still send after acknowledging below; …"; hardware on-device review.
- (f) V3-Vault-Advanced (PSBTs) — partial (signer matrix / Border reconstruction not named).

### 9.4 PSBTs list — "PSBT is imported", "No PSBTs yet", "Your pending spend transactions will appear here once you create or import PSBTs.", "Self-transfer". (f) V3-Vault-Advanced.

### 9.5 Receive (`vault/receive.rs`)
- (c) "Generate address", "Show QR Code", "Verify on hardware device", "Select device to verify address on:", "Address:", "Derivation index:", "No addresses yet", "Generate a new address to receive bitcoin. Always generate a new address for each deposit.", "Previously generated addresses still awaiting deposit", "Loading addresses"; label editor "Add label" ("Invalid label length, cannot be superior to 100").
- (e) Hardware address verification ("Tenshu cannot request the device to display the address. \n The verification must be done manually with the device control."). (f) V3-Wallet-Receive — partial (HW verify not named).

### 9.6 Coins (`vault/coins.rs`)
- (c) "No coins yet", "Your coins will appear here once you receive bitcoin.", "One or more recovery paths are available", "First recovery path will be available in {} blocks", detail "Address label:", "No label", "Deposit transaction label:", "Outpoint:", "Block height:", "Spend txid:", "Spend block height:", "Not in a block", "Refresh coin", expiry "Expires today" / "Expires in ≈ 2 days"; Blake2b replay badges "Seen on both chains", "Replay status not checked", "Replay protection unverified", "Replay-protected split deposit".
- (f) V3-Vault-Advanced (coins).

### 9.7 Transactions (+ RBF)
- (c) "Loading transactions", "No transactions yet", "Incoming transaction"/"Outgoing transaction"/"Self-transfer", "Miner fee: ", "Bump fee", "Cancel transaction" ("Best effort attempt at double spending an unconfirmed outgoing transaction"), "Transaction replacement", "Feerate must be greater than previous value and less than or equal to 1000 sats/vbyte", "Replacement PSBT created successfully and ready to be signed", "Go to replacement"; "WARNING: Replacing this transaction will invalidate some later payments.".
- (f) V3-Activity — partial (RBF not named).

### 9.8 Recovery (`vault/recovery.rs`)
- (c) "Recover your funds by sending them to another wallet if you have lost access to your primary spending path.", "No recovery path is currently available.", "{} recovery path{} available:", "{} signature{} from", "{} coin{} totalling".
- (d) Hidden for imported wallets. (f) V3-Vault-Advanced (recovery path).

### 9.9 Claim BTCB2 (`vault/claim.rs`)
- (c) "Claim Bitcoin Blake2b", stepper "1. Deposits" / "2. Review and sign" / "3. Broadcast and confirm", "Deposit addresses" / "Hide deposit addresses", "Bitcoin · minimum {} sats", "Bitcoin Blake2b · minimum {} sats", "Verifying deposits…", "Retry verification", "Pause checks", "Funds received at both addresses", "Both deposits verified for replay protection", "Coins to split", "Fee rate", "{rate} sat/vB (about an hour).", "Review and sign", "Sign with this Vault's keys", "Review Bitcoin broadcast", "Bitcoin — broadcast now", "Bitcoin Blake2b — recorded, not sent now", "Broadcast Transaction", "Final review — this is the last step before broadcast", "Broadcasting sends this transaction to the Bitcoin network. It cannot be taken back. …", "Confirm resend of original transaction", "Acknowledge new confirmation", "Continue in Bitcoin Blake2b"; replay-verification states ("Replay protection not verified yet.", "Indexer rate limited. Retry verification after the cooldown.", …).
- (d) Blake2b grant + beta + Connect sign-in; "Split signing is disabled for this launch. Restart Tenshu with --enable-btcb2-split to enable live testing.".
- (e) Two-chain recheck before each broadcast, six-confirmation gate, never auto-resend.
- (f) V3-Vault-Split — partial (claim wizard vs split naming).

### 9.10 Split (step 1 / step 2 / sweep; `vault/split.rs`)
- (c) "Split: sign both transactions", "Split: step 1 on Bitcoin", progress lines ("Building step 1…", "Submitting through Connect…", "Checking step 1's confirmations on both chains…", "Building step 2…", …), "Reserve address", "Build step 2", "Back to step 1", "Save PSBT", "Save as text", "Import signed", "Review step 2", "Submit step 2", "Send again" / "Review resend", "Acknowledge new block", "Complete split"; sweep: "Recovery phrase", "Passphrase (leave empty if none)", "Add phrase", "Clear phrases", "Build and sign", "Review sweep", "Submit sweep"; hardware "Sign with a connected hardware wallet", "Confirm on your hardware wallet. Check the amounts and the address on its screen before you approve.", "Sign Bitcoin on device" / "Sign BTCB2 on device".
- (e) Recovery phrases kept in memory only and cleared after signing; recorded submissions never resent automatically.
- (f) V3-Vault-Split.

### 9.11 Settings → Node (`vault/settings/mod.rs`)
- (c) "Bitcoin node", "Active backend", "Switch to COINCUBE | Connect" / "Switch to local node" / "Set up local node", "How would you like to connect to Bitcoin Core?" — "My own node" ("Configure →") / "Tenshu-managed node (recommended)" ("Bitcoin Knots →" / "Bitcoin Core →"), "Switch node software to {}?", "Your Vault is still scanning" ("Keep scanning" / "Switch anyway"), local-node progress ("Local node syncing", "Progress {:.1}%", "Verified blocks: {blocks} / {headers} known headers · {} blocks remaining"), own-node form ("Bitcoin Core RPC address", "Cookie file" / "Username / Password", "Add local node"), "Blockchain rescan" ("Year:", "Month:", "Day:", "Start rescan", "Provided date is in the future"), Tor "Inbound connections" ("Accept inbound connections over Tor", "Limit upload to ~1 GB/day", "Also route outbound connections through Tor", "Onion address: {onion}", "Restart node to apply"), "Your own Esplora" ("Check and use", "No fallback is kept: …"), history recovery ("Recover history to switch to your local node", "Recommended: Import Connect history", "Recover from this height or date", "Pause after this batch", "Resume recovery", "Cancel recovery", "Keep using Connect"), "Block retention", "Node resources" ("Lowering the target prunes more block data now, and that can't be undone.").
- (e) Confirmation before node-software switch / scan abort; prune-irreversibility warning.
- (f) Cube-Workspace (presumed Node section) — partial; P-Settings (by name).

### 9.12 Settings → Wallet
- (c) "Wallet descriptor:", "Vault ID:"/"Wallet ID:" ("A stable 4-byte identifier derived from this Vault's descriptor. Used to scope paired local signers."), "Back up encrypted descriptor", "Register on hardware device", "Wallet alias:", "Fingerprint aliases:", "The wallet policy:", "{} signature{}", "(Recovery path #{})"; sharing "Grant access to wallet to another user", "User email", "Send invitation", "Invitation was sent"; also "Fiat price:" / "Exchange rate source:" / "Currency:" (`settings/general.rs`).
- (f) V3-Vault-Advanced (descriptors & keys); Cube-Workspace.

### 9.13 Settings → Import/Export (+ export progress, `vault/export.rs`)
- (c) "Import/Export", "A collection of the export and import functions present in Tenshu.", "Connect Recovery Kit", "Encrypted descriptor" (".bed file, can be decrypted with one of your signing devices or xpubs."), "Descriptor only - plain-text", "Transactions table" (".CSV file …"), "BIP 329 labels", "Export wallet", "Import wallet", "Other formats"; progress "Select the path you want to export in the popup window...", "Progress: {}%", "Export failed: timeout", "Export canceled", conflict prompts "Labels conflict, what do you want to do?", "Aliases conflict, what do you want to do?".
- (f) Cube-Workspace — partial.

### 9.14 Settings → Pair (LocalSigning; `settings/local_signing.rs`)
- (c) "Pair with Keychain", "Pair a Keychain phone over your local network so it can sign PSBTs directly, without going through the Connect API. The phone must be on the same Wi-Fi.", "Pair phone", "The Keychain key this phone will sign for:", "Show key details"/"Hide key details", "No Keychain keys to pair", "Pick a phone to pair with", "Looking for phones…", "Phone {}", "Pair", "Scan this QR with the Keychain app on phone {} ({}).", "Waiting for {} — expires in {}s"; failures "Offer expired", "Wrong wallet", "QR already used", "Couldn't verify the phone", "Keychain needs updating", "Network error", "Pairing failed", "Try again"; "Paired phones" ("No paired phones for this vault yet. …", "Phone name", "Fallback host:port (mDNS-blocked networks)", "Save", "Remove", "Exact vault key paired").
- (e) TLS + phone identity proof; wallet-id match. (f) Cube-Workspace — partial.

### 9.15 Fiat/labels/warnings
- (c) Fiat tooltip "Exchange rate: 1 BTC ~ {} {}", "Source: {}", "Last updated at source: {}", "BTCB2 quote is unavailable or stale"; label editing "(External Output)".
- (f) none specific.

---

## 10. Shared components
- Feerate picker: "Slow (~4h)", "Normal (~1h)", "Fast (~10m)", placeholder "42 (in sats/vbyte)", "Feerate must be an integer between 1 and 1000 sats/vbyte". (f) V3-Wallet-Send — partial.
- Recipient identity (Branta): "Recipient identified", "View with Branta"; enabled via Cube General "Recipient identity checks". (f) none.
- Generic picker (`shared/picker.rs`), wallet header sync line, network banner, "< Back"/"Close" modal chrome, "Not available on this network". (f) none.
- Nav rails: primary (5 top-levels) → secondary (per-wallet sub-items above) → tertiary (Cube settings: "General", "Recovery", "Stats", "Avatar", "About", "Members"; Spark: "General", "Lightning Address"; P2P: "Book", "My Trades", "Chat", "Create", "Settings"; Vault: "Node", "Wallet", "Import/Export", "Pair"). Disabled items show the availability reason popover. (f) V3-Cube-Overview / Cube-Workspace — partial.

---

## COVERAGE MATRIX

Updated 2026-10-23 against the V6 artboards, including the three V6-Gaps boards (Installer, Wallets, Account), plus P-Settings and the V3 Vault/Transfer/Send/Activity boards). "Existing artboard(s)" lists every board that now draws the feature; "V6 artboard" names the new V6 file(s) only.

| # | Feature | Existing artboard(s) | V6 artboard | Status | Missing (one line) |
|---|---|---|---|---|---|
| 1 | Launcher Cube list + row statuses | Launcher-3D, Launcher-Iso, V2-Launcher-3D, V2-Launcher-Faces, V3-Launcher-Spin, V6-Cube-Delete, V6-Cube-Restore, V6-Gaps-Wallets | V6-Cube-Delete, V6-Cube-Restore (remote Cubes "On your Connect account"), V6-Gaps-Wallets (launcherRestore) | covered | — |
| 2 | Network dropdown (dev mode / Blake2b) | Launcher-States, V6-Cube-Delete, V6-Cube-Create, V6-Gaps-Installer | V6-Cube-Delete (Network ▾ all/live/test), V6-Cube-Create (network step, dev-mode and Blake2b grant gates), V6-Gaps-Installer (chainUnavailable, blake2bMode) | covered | — |
| 3 | Global Settings (Developer mode, Blake2b beta) | V3-Launcher-AppPrefs | — | covered | — |
| 4 | Launcher sidebar (Cubes/Connect/Settings/Sign In) | V3-Launcher-Spin, V3-Launcher-Connect, V6-Connect-SignIn, V6-Cube-Restore, V6-Gaps-Details | V6-Connect-SignIn, V6-Cube-Restore ("Recover a Vault" via Import or restore); V6-Gaps-Details (splitEntry) | covered | — (was: "Split a Bitcoin wallet" launcher entry) |
| 5 | Create Cube form (PIN/passkey) | Journey, V6-Cube-Create | V6-Cube-Create | covered | — |
| 6 | Creation backup gate (phrase/kit/bypass) | V6-Cube-Create | V6-Cube-Create | covered | — |
| 7 | Passkey registration overlay | V6-Cube-Create | V6-Cube-Create | covered | — |
| 8 | Rename / Delete Cube / Delete Remote Cube | V6-Cube-Delete, P-Settings (Cube name) | V6-Cube-Delete | covered | — |
| 9 | Recover choice (password vs phone) | V3-Launcher-Connect, V6-Cube-Restore | V6-Cube-Restore | covered | — |
| 10 | Liquid mnemonic restore | V6-Cube-Restore | V6-Cube-Restore (phrase path, tagged "No entry point in this build") | covered | — (screen has no entry point in the app) |
| 11 | Import Bitcoin wallet (descriptors/HW, server, PSBT round-trip) | V6-Cube-Restore | V6-Cube-Restore | covered | — |
| 12 | Recover a Vault (heir list) | V3-Launcher-Connect, V6-Cube-Restore | V6-Cube-Restore | covered | — |
| 13 | Split a Bitcoin wallet (scan) | V3-Vault-Split, V6-Gaps-Installer | V6-Gaps-Installer (split) | covered | — |
| 14 | Installer: template picker | V6-Cube-Create | V6-Cube-Create | covered | — |
| 15 | Installer: template intro + diagram | V6-Cube-Create | V6-Cube-Create | covered | — |
| 16 | Installer: descriptor editor / timelock / threshold | V3-Vault-Advanced, V6-Cube-Create | V6-Cube-Create (Set keys) | covered | — |
| 17 | Installer: key-source picker (Cube Key/HW/Keychain/xpub) | V3-Vault-Advanced, V6-Cube-Create, V6-Gaps-Installer | V6-Cube-Create, V6-Gaps-Installer (keySource) | covered | — |
| 18 | Border Wallet wizard (enrol) | V6-BorderWallet | V6-BorderWallet | covered | — |
| 19 | Installer: backup master seed | Journey, V6-Cube-Create | V6-Cube-Create | covered | — |
| 20 | Installer: register / back up descriptor | V6-Gaps-Installer | V6-Gaps-Installer (register) | covered | — |
| 21 | Installer: Connect step (create/sign-in/OTP/skip) | V3-Launcher-Connect, V6-Cube-Create, V6-Connect-SignIn, V6-Gaps-Installer | V6-Cube-Create (Connect registration), V6-Connect-SignIn, V6-Gaps-Installer (connectStep) | covered | — |
| 22 | Installer: backend/node choice + managed node install | V6-Cube-Create | V6-Cube-Create | covered | — |
| 23 | Installer: own node (bitcoind/Electrum/Esplora) form | V6-Cube-Create | V6-Cube-Create | covered | — |
| 24 | Installer: Recovery Kit restore | V3-Launcher-Connect, V6-Cube-Restore | V6-Cube-Restore | covered | — |
| 25 | Installer: owner keychain restore (phone) | V3-Launcher-Connect, V6-Cube-Restore | V6-Cube-Restore | covered | — |
| 26 | Installer: inheritance restore | V6-Cube-Restore | V6-Cube-Restore (inherited Vault) | covered | — |
| 27 | Installer: import descriptor / shared wallet / .bed decrypt | V6-Cube-Restore, V6-Gaps-Installer | V6-Cube-Restore (public-descriptor import only), V6-Gaps-Installer (importExtras) | covered | — |
| 28 | Installer: PIN setup | V3-Launcher-Spin, V6-Cube-Restore, V6-Cube-Create | V6-Cube-Restore, V6-Cube-Create | covered | — |
| 29 | Installer: wallet alias + Final | V6-Cube-Create, V6-Cube-Restore | V6-Cube-Create, V6-Cube-Restore | covered | — |
| 30 | Installer: Claim overview (Blake2b) | V3-Vault-Split, V6-Claim | V6-Claim | covered | — |
| 31 | Loader stages + error/retry | V3-Launcher-Spin, Launcher-States, V6-Cube-Opening | V6-Cube-Opening | covered | — |
| 32 | Loader quotes (65, attributions, images) | V6-Cube-Opening, V6-Cube-Unlock | V6-Cube-Opening, V6-Cube-Unlock | covered | — |
| 33 | PinEntry unlock | V3-Launcher-Spin, V6-Cube-Unlock | V6-Cube-Unlock | covered | — |
| 34 | PasskeyUnlock | V3-Launcher-Spin, V6-Cube-Unlock | V6-Cube-Unlock | covered | — |
| 35 | Login (remote backend token) | V3-Launcher-Connect, V6-Connect-SignIn, V6-Gaps-Installer | V6-Connect-SignIn (Connect email/token only), V6-Gaps-Installer (legacyLogin) | covered | — |
| 36 | DuressActive screen | V6-Duress | V6-Duress | covered | — |
| 37 | Cube Overview (wallet cards, total, backup nag) | Cube-Compact, V2-Cube-Overview, V2-Cube-Compact, V3-Cube-Overview, V6-Gaps-Details | V6-Gaps-Details (stableBalance) | covered | — (was: Stable-balance and USDt-pending banners) |
| 38 | Transfer between wallets | V3-Cube-Transfer | — | covered | — |
| 39 | Cube Settings → General | Cube-Workspace, P-Settings | — | covered | — |
| 40 | Cube Settings → Recovery (seed backup, Recovery Kit, Recovery Alerts) | Cube-Workspace, V3-Launcher-Connect, P-Settings, V6-Gaps-Account | V6-Gaps-Account (kitMode, alerts) | covered | — |
| 41 | Cube Settings → About / Stats | Cube-Workspace | — | covered | — |
| 42 | Cube Settings → Avatar generator | Cube-Workspace, P-Settings, V6-Gaps-Account | V6-Gaps-Account (avatar) | covered | — |
| 43 | Cube Settings → Members (flagged) | Cube-Workspace, V6-Members | V6-Members | covered | — |
| 44 | Connect sign-in / sign-up (promo) / OTP | V3-Launcher-Connect, V6-Connect-SignIn | V6-Connect-SignIn | covered | — |
| 45 | Connect Overview | V3-Launcher-Connect, V6-Gaps-Details | V6-Gaps-Details (connectBanners); V3-Launcher-Connect planState lever ("Access until") | covered | — |
| 46 | Connect Plan & Billing + checkout + history | V3-Launcher-Connect, V6-Gaps-Details | V6-Gaps-Details (checkout) | covered | — (was: Lightning/on-chain invoice checkout states) |
| 47 | Connect Security (devices, login activity) | V3-Launcher-Connect | headings now the app's "Verified Devices" / "Login Activity" | covered | — |
| 48 | Connect Contacts | V3-Launcher-Connect, V6-Gaps-Details | V6-Gaps-Details (addToCubes) | covered | — (was: Add-to-Cube(s) dialog) |
| 49 | Duress: status + enrol wizard + disable + clear | V6-Duress | V6-Duress | covered | — |
| 50 | Duress emergency contacts | V6-Duress | V6-Duress | covered | — |
| 51 | Buy/Sell panel (sign-in, country, mode) | V6-Market-BuySell | V6-Market-BuySell | covered | — |
| 52 | Buy/Sell Mavapay (NGN/ZAR/KES) | V6-Market-BuySell | V6-Market-BuySell | covered | — |
| 53 | Buy/Sell Meld (webview) | V6-Market-BuySell | V6-Market-BuySell (provider page drawn, not embedded) | covered | — |
| 54 | P2P order book + filters + take order | V6-Market-P2P | V6-Market-P2P | covered | — |
| 55 | P2P create order | V6-Market-P2P | V6-Market-P2P | covered | — |
| 56 | P2P trade lifecycle + Spark hold-invoice pay + rating | V6-Market-P2P | V6-Market-P2P | covered | — |
| 57 | P2P chat (incl. dispute, attachments) | V6-Market-P2P | V6-Market-P2P | covered | — |
| 58 | P2P settings (Mostro nodes, relays) | V6-Market-P2P | V6-Market-P2P | covered | — |
| 59 | Liquid Overview | V3-Cube-Overview, V3-Activity, V6-Liquid | V6-Liquid | covered | — |
| 60 | Liquid Send (incl. USDt, fee asset, fiat entry) | V3-Wallet-Send | — | covered | — |
| 61 | Liquid SideShift send (USDt cross-chain) | V3-Wallet-Send, V6-Gaps-Wallets | V6-Gaps-Wallets (sideshiftSend) | covered | — |
| 62 | Liquid Receive (LN/L-BTC/USDt/on-chain swap, SideShift) | V3-Wallet-Receive, V6-Wallet-Receive | V6-Wallet-Receive | covered | — |
| 63 | Liquid Swap (L-BTC ↔ USDt) | V6-Liquid | V6-Liquid | covered | — |
| 64 | Liquid Transactions + refund | V3-Activity, V6-Liquid | V6-Liquid | covered | — |
| 65 | Spark Overview / bridge states | V3-Cube-Overview, P-Settings, V6-Gaps-Wallets | V6-Gaps-Wallets (sparkBridge) | covered | — |
| 66 | Spark Send (incl. cross-chain stablecoin, quotes) | V3-Wallet-Send, V6-Gaps-Details | V6-Gaps-Details (sparkGuard) | covered | — (was: Unconfirmed-status and address-mismatch guard) |
| 67 | Spark Receive (LN, Spark address, on-chain deposits, SideShift) | V3-Wallet-Receive, V6-Wallet-Receive, V6-Gaps-Wallets | V6-Wallet-Receive, V6-Gaps-Wallets (sparkDeposits) | covered | — |
| 68 | Spark Transactions | V3-Activity | sparkHistory lever: loading row, failed banner | covered | — |
| 69 | Spark Settings → General (Stable Balance, bridge status) | Cube-Workspace, P-Settings | — | covered | — |
| 70 | Spark Settings → Lightning Address | Cube-Workspace, P-Settings | — | covered | — |
| 71 | Vault Overview (sync/recovery-path banners) | V3-Cube-Overview, V3-Activity, V6-Gaps-Wallets | V6-Gaps-Wallets (vaultOverview) | covered | — |
| 72 | Vault Send (recipients, coin selection, feerate) | V3-Wallet-Send, V6-Gaps-Details | V6-Gaps-Details (vaultBatch) | covered | — (was: Batch label / coin-selection detail) |
| 73 | Vault PSBT detail + signer matrix + broadcast | V3-Vault-Advanced, V6-Signing, V6-BorderWallet | V6-Signing (hardware, Keychain via Connect/Wi-Fi), V6-BorderWallet (Border signing) | covered | — |
| 74 | Vault PSBTs list | V3-Vault-Advanced | — | covered | — |
| 75 | Vault Receive (+ HW address verify, labels) | V3-Wallet-Receive, V6-Wallet-Receive | V6-Wallet-Receive | covered | — |
| 76 | Vault Coins (+ replay badges) | V3-Vault-Advanced | per-row Details: Outpoint, Spend txid | covered | — |
| 77 | Vault Transactions + RBF bump/cancel | V3-Activity, V6-Gaps-Wallets | V6-Gaps-Wallets (rbf) | covered | — |
| 78 | Vault Recovery (recovery paths) | V3-Vault-Advanced | — | covered | — |
| 79 | Vault Claim BTCB2 wizard | V3-Vault-Split, V6-Claim | V6-Claim | covered | — |
| 80 | Vault Split (step 1/2, sweep, HW signing) | V3-Vault-Split | — | covered | — |
| 81 | Vault Settings → Node (switch, Tor, Esplora, history recovery, retention) | Cube-Workspace, P-Settings, V6-Gaps-Wallets | V6-Gaps-Wallets (nodeSettings) | covered | — |
| 82 | Vault Settings → Wallet (descriptor, aliases, register, share) | V3-Vault-Advanced, Cube-Workspace, P-Settings, V6-Gaps-Details | V6-Gaps-Details (shareWallet) | covered | — (was: Share-wallet invitation) |
| 83 | Vault Settings → Import/Export + progress | Cube-Workspace, P-Settings, V6-Signing | V6-Signing (export progress, timeout, label/alias conflicts) | covered | — |
| 84 | Vault Settings → Pair (LAN Keychain) | Cube-Workspace, P-Settings, V6-Signing | V6-Signing | covered | — |
| 85 | Hardware firmware advisories (Coldcard/BitBox02) | V6-Signing | V6-Signing | covered | — |
| 86 | Feerate picker (shared) | V3-Wallet-Send, V6-Liquid (refund), V6-Gaps-Wallets | V6-Liquid, V6-Gaps-Wallets (feerate) | covered | — |
| 87 | Branta recipient identity (shared) | V6-Signing, P-Settings, V3-Wallet-Send, V3-Vault-Advanced | V3-Wallet-Send recipientIdentity lever (identified card, tampered mismatch, checks off); V3-Vault-Advanced psbtIdentity (identity review before broadcast) | covered | — |
| 88 | Nav rails incl. disabled-with-reason popovers | V3-Cube-Overview, Cube-Workspace, P-Settings, V6-Market-P2P, V6-Market-BuySell, V6-Wallet-Receive | V6-Market-P2P, V6-Market-BuySell, V6-Wallet-Receive (network/flag gates) | covered | — |

Counts: covered 88, partial 0, none 0 (88 rows). Previous (pre-V6): covered 26, partial 33, none 29; after the first V6 pass: covered 73, partial 14, none 1.

Remaining gaps:
- none: no rows.
- partial: no rows. Some covered rows still note minor detail gaps in "Missing": #4 "Split a Bitcoin wallet" launcher entry; #37 stable-balance and USDt-pending banners; #45 renewal/expired banners; #46 Lightning/on-chain invoice checkout states; #48 Add-to-Cube(s) dialog; #66 unconfirmed-status and address-mismatch guard; #72 batch label / coin-selection detail; #82 share-wallet invitation (from Settings).
