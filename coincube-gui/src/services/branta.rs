//! Send-only recipient identification. No wallet state or payment amounts cross this boundary.
//! SDK errors and unrecognised requests are silent; only authenticated `Tampered` blocks a send.
use async_trait::async_trait;
use branta::{
    BrantaClientOptions, BrantaError, BrantaServerBaseUrl, BrantaService, DestinationType,
    PrivacyMode,
};
use coincube_core::miniscript::bitcoin::{Address, Network};
use iced::{
    futures::{stream, StreamExt},
    widget::image::Handle,
};
use std::{
    collections::VecDeque,
    fmt,
    io::Cursor,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use tokio::sync::watch;

const TIMEOUT: Duration = Duration::from_secs(3);
const MAX_LOGO_BYTES: usize = 256 * 1024;
const MAX_LOGO_DIMENSION: u32 = 5000;
const LOGO_THUMBNAIL_DIMENSION: u32 = 64;
const MAX_REQUEST_BYTES: usize = 16 * 1024;
const MAX_IDENTITIES: usize = 4;
pub const MISMATCH_MESSAGE: &str =
    "This payment request’s address does not match its verification data. Do not send.";

#[derive(Clone, Copy)]
struct Preference {
    enabled: bool,
    epoch: u64,
}
struct Privacy {
    state: watch::Sender<Preference>,
}
impl Privacy {
    fn new(enabled: bool) -> Self {
        Self {
            state: watch::channel(Preference { enabled, epoch: 0 }).0,
        }
    }
    fn set(&self, enabled: bool) {
        self.state.send_if_modified(|s| {
            if s.enabled == enabled {
                return false;
            }
            s.enabled = enabled;
            s.epoch = s.epoch.wrapping_add(1);
            true
        });
    }
}
static PRIVACY: OnceLock<Arc<Privacy>> = OnceLock::new();
fn privacy() -> &'static Arc<Privacy> {
    PRIVACY.get_or_init(|| Arc::new(Privacy::new(true)))
}
/// Called before constructing any wallet panels. Does not initialise an SDK client.
pub fn set_enabled(enabled: bool) {
    privacy().set(enabled);
}
pub fn enabled() -> bool {
    privacy().state.borrow().enabled
}
pub fn endpoint(network: Network) -> BrantaServerBaseUrl {
    if network == Network::Bitcoin {
        BrantaServerBaseUrl::Production
    } else {
        BrantaServerBaseUrl::Staging
    }
}

/// Sensitive input deliberately has no derived Debug or serialization implementation.
#[derive(Clone)]
pub struct LookupRequest {
    raw: String,
    destination: String,
    kind: DestinationType,
}
impl fmt::Debug for LookupRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LookupRequest([redacted])")
    }
}
impl LookupRequest {
    pub fn bolt11(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if !raw.is_ascii() || raw.len() > MAX_REQUEST_BYTES {
            return None;
        }
        let dest = if raw
            .get(..10)
            .is_some_and(|s| s.eq_ignore_ascii_case("lightning:"))
        {
            &raw[10..]
        } else {
            raw
        };
        // Restrict to the route Tenshu will actually prepare. No URI alternate destinations.
        if dest.contains(['?', '#', '&', ':']) || !dest.to_ascii_lowercase().starts_with("ln") {
            return None;
        }
        let parser = branta::QrParser::new(raw);
        if parser.destination_type() != Some(DestinationType::Bolt11) {
            return None;
        }
        Some(Self {
            raw: raw.into(),
            destination: dest.to_ascii_lowercase(),
            kind: DestinationType::Bolt11,
        })
    }
    pub fn bitcoin(raw: &str, address: &Address) -> Option<Self> {
        let raw = raw.trim();
        if !raw.is_ascii()
            || !safe_percent_encoding(raw)
            || raw.len() > MAX_REQUEST_BYTES
            || !raw.get(..8)?.eq_ignore_ascii_case("bitcoin:")
            || raw.contains('#')
        {
            return None;
        }
        let parser = branta::QrParser::new(raw);
        if !parser.is_on_chain_zk()
            || parser.destination_type() != Some(DestinationType::BitcoinAddress)
        {
            return None;
        }
        let parsed: Address<coincube_core::miniscript::bitcoin::address::NetworkUnchecked> =
            parser.destination()?.parse().ok()?;
        if parsed.assume_checked().script_pubkey() != address.script_pubkey() {
            return None;
        }
        // Reject ambiguous parameters before the SDK's last-key-wins parser sees them.
        let mut keys = std::collections::HashSet::new();
        for pair in raw.split_once('?')?.1.split('&') {
            let key = pair.split('=').next()?.to_ascii_lowercase();
            if key.contains('%') || !keys.insert(key) {
                return None;
            }
        }
        if parser.on_chain_encryption_secret.as_ref()?.is_empty() {
            return None;
        }
        // The SDK sends branta_id as supplied. Ensure this is an encrypted
        // envelope, not a plaintext address placed in the ID field.
        use base64::Engine;
        let envelope = base64::engine::general_purpose::STANDARD
            .decode(parser.on_chain_encryption_text.as_ref()?)
            .ok()?;
        if envelope.len() < 28 {
            return None;
        }
        // Upstream only normalizes mainnet bech32 during its typed tamper check.
        // An uppercase testnet URI cannot be compared reliably by this version.
        let original = parser.destination()?;
        if !original.to_ascii_lowercase().starts_with("bc1")
            && (original.to_ascii_lowercase().starts_with("tb1")
                || original.to_ascii_lowercase().starts_with("bcrt1"))
            && original != original.to_ascii_lowercase()
        {
            return None;
        }
        Some(Self {
            raw: raw.into(),
            destination: address.to_string(),
            kind: DestinationType::BitcoinAddress,
        })
    }
}

#[derive(Clone)]
pub struct RecipientIdentity {
    pub platform: String,
    pub description: Option<String>,
    pub logo: Option<Handle>,
    pub logo_light: Option<Handle>,
    verify_url: String,
    privacy: Arc<Privacy>,
    epoch: u64,
}
impl fmt::Debug for RecipientIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RecipientIdentity([redacted])")
    }
}
impl RecipientIdentity {
    pub fn is_current(&self) -> bool {
        let s = self.privacy.state.borrow();
        s.enabled && s.epoch == self.epoch
    }
    pub fn open(&self) {
        if self.is_current() {
            let _ = crate::browser::open_url(&self.verify_url);
        }
    }
}
#[derive(Clone, Debug, Default)]
pub enum LookupResult {
    #[default]
    Silent,
    Identified(Vec<RecipientIdentity>),
    Tampered,
}

/// Narrow, injectable seam: automated tests never instantiate the official network backend.
#[async_trait]
pub trait LookupBackend: Send + Sync {
    async fn lookup(
        &self,
        network: Network,
        request: &LookupRequest,
    ) -> Result<branta::PaymentsResult, BrantaError>;
    async fn logo(&self, network: Network, url: &str) -> Option<Handle>;
}
struct OfficialBackend;
#[async_trait]
impl LookupBackend for OfficialBackend {
    async fn lookup(
        &self,
        network: Network,
        request: &LookupRequest,
    ) -> Result<branta::PaymentsResult, BrantaError> {
        let service = BrantaService::new(BrantaClientOptions {
            base_url: endpoint(network),
            privacy: PrivacyMode::Strict,
            default_api_key: None,
            hmac_secret: None,
        });
        // Complete original request is processed locally by the SDK. Strict mode transmits
        // only its ZK lookup value; no receive-side APIs or credentials are configured.
        service.get_payments_by_qr_code(&request.raw, None).await
    }
    async fn logo(&self, network: Network, url: &str) -> Option<Handle> {
        fetch_logo(network, url).await
    }
}
#[derive(Clone)]
pub struct LookupTicket {
    privacy: Arc<Privacy>,
    epoch: u64,
    network: Network,
    backend: Arc<dyn LookupBackend>,
}
impl fmt::Debug for LookupTicket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LookupTicket")
    }
}
pub fn begin(network: Network) -> Option<LookupTicket> {
    ticket(privacy().clone(), network, Arc::new(OfficialBackend))
}
fn ticket(
    privacy: Arc<Privacy>,
    network: Network,
    backend: Arc<dyn LookupBackend>,
) -> Option<LookupTicket> {
    let state = *privacy.state.borrow();
    state.enabled.then_some(LookupTicket {
        privacy,
        epoch: state.epoch,
        network,
        backend,
    })
}
impl LookupTicket {
    pub fn is_current(&self) -> bool {
        let s = self.privacy.state.borrow();
        s.enabled && s.epoch == self.epoch
    }
    pub async fn lookup(&self, request: LookupRequest) -> LookupResult {
        let mut changes = self.privacy.state.subscribe();
        if !self.is_current() {
            return LookupResult::Silent;
        }
        tokio::select! {
            biased;
            _ = changes.changed() => LookupResult::Silent,
            result = self.lookup_inner(request, tokio::time::Instant::now() + TIMEOUT) => {
                if self.is_current() { result } else { LookupResult::Silent }
            }
        }
    }
    /// Bounded fanout, preserving the caller's row IDs including duplicate destinations.
    /// One deadline covers the entire batch, not three seconds per wave.
    pub async fn lookup_many(
        &self,
        requests: Vec<(usize, LookupRequest)>,
    ) -> Vec<(usize, LookupResult)> {
        let mut changes = self.privacy.state.subscribe();
        if !self.is_current() {
            return Vec::new();
        }
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        let mut pending = stream::iter(
            requests
                .into_iter()
                .map(|(id, request)| async move { (id, self.identify(request, deadline).await) }),
        )
        .buffer_unordered(3);
        let mut results = Vec::new();
        loop {
            tokio::select! {
                biased;
                _ = changes.changed() => return Vec::new(),
                next = pending.next() => match next { Some(item) => results.push(item), None => break }
            }
        }
        // Identify every recipient before optional images can consume the batch budget.
        let logos = stream::iter(
            results
                .into_iter()
                .map(|(id, (mut result, urls))| async move {
                    self.load_logos(&mut result, urls, deadline).await;
                    (id, result)
                }),
        )
        .buffer_unordered(3)
        .collect::<Vec<_>>();
        let results = tokio::select! {
            biased;
            _ = changes.changed() => return Vec::new(),
            results = logos => results,
        };
        if self.is_current() {
            results
        } else {
            Vec::new()
        }
    }
    async fn lookup_inner(
        &self,
        request: LookupRequest,
        deadline: tokio::time::Instant,
    ) -> LookupResult {
        let (mut result, urls) = self.identify(request, deadline).await;
        self.load_logos(&mut result, urls, deadline).await;
        if self.is_current() {
            result
        } else {
            LookupResult::Silent
        }
    }
    async fn identify(
        &self,
        request: LookupRequest,
        deadline: tokio::time::Instant,
    ) -> (LookupResult, Vec<(Option<String>, Option<String>)>) {
        if !self.is_current() || tokio::time::Instant::now() >= deadline {
            return (LookupResult::Silent, Vec::new());
        }
        let response =
            match tokio::time::timeout_at(deadline, self.backend.lookup(self.network, &request))
                .await
            {
                Ok(Ok(r)) => r,
                Ok(Err(BrantaError::Tampered)) => return (LookupResult::Tampered, Vec::new()),
                _ => return (LookupResult::Silent, Vec::new()),
            };
        if !self.is_current() || !same_origin(self.network, &response.verify_url) {
            return (LookupResult::Silent, Vec::new());
        }
        let mut identities = Vec::new();
        let mut logo_urls = Vec::new();
        for payment in response
            .payments
            .into_iter()
            .filter(|p| {
                p.destinations.iter().any(|d| {
                    d.is_zk
                        && !d.is_encrypted
                        && d.r#type == Some(request.kind)
                        && destination_matches(&d.value, &request.destination, request.kind)
                })
            })
            .take(MAX_IDENTITIES)
        {
            let Some(platform) = payment.platform.as_deref().filter(|s| !s.trim().is_empty())
            else {
                continue;
            };
            if !self.is_current() {
                return (LookupResult::Silent, Vec::new());
            }
            logo_urls.push((payment.platform_logo_url, payment.platform_logo_light_url));
            identities.push(RecipientIdentity {
                platform: bounded_text(platform, 80),
                description: payment
                    .description
                    .as_deref()
                    .map(|s| bounded_text(s, 200))
                    .filter(|s| !s.is_empty()),
                logo: None,
                logo_light: None,
                verify_url: response.verify_url.clone(),
                privacy: self.privacy.clone(),
                epoch: self.epoch,
            });
        }
        if !self.is_current() || identities.is_empty() {
            (LookupResult::Silent, Vec::new())
        } else {
            (LookupResult::Identified(identities), logo_urls)
        }
    }
    async fn load_logos(
        &self,
        result: &mut LookupResult,
        urls: Vec<(Option<String>, Option<String>)>,
        deadline: tokio::time::Instant,
    ) {
        let LookupResult::Identified(identities) = result else {
            return;
        };
        if !self.is_current() || tokio::time::Instant::now() >= deadline {
            return;
        }
        let logos = async {
            let requests = urls
                .into_iter()
                .enumerate()
                .flat_map(|(index, (dark, light))| [(index, false, dark), (index, true, light)]);
            let mut pending = stream::iter(requests.map(|(index, light, url)| async move {
                (index, light, self.load_logo(url.as_deref()).await)
            }))
            .buffer_unordered(MAX_IDENTITIES * 2);
            // Preserve each completed variant even if another one misses the deadline.
            while let Some((index, light, handle)) = pending.next().await {
                if light {
                    identities[index].logo_light = handle;
                } else {
                    identities[index].logo = handle;
                }
            }
        };
        let _ = tokio::time::timeout_at(deadline, logos).await;
    }
    async fn load_logo(&self, url: Option<&str>) -> Option<Handle> {
        let url = url?;
        if !self.is_current() || !same_origin(self.network, url) {
            return None;
        }
        // lookup_inner bounds all optional logos by the remaining shared deadline.
        self.backend.logo(self.network, url).await
    }
}
// SDK query parsing uses byte slices for type detection. Reject malformed escapes
// and decoded non-ASCII before it can inspect alternate destinations.
fn safe_percent_encoding(raw: &str) -> bool {
    let mut bytes = raw.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let Some(a) = bytes.next().and_then(|b| (b as char).to_digit(16)) else {
                return false;
            };
            let Some(b) = bytes.next().and_then(|b| (b as char).to_digit(16)) else {
                return false;
            };
            if a * 16 + b > 127 {
                return false;
            }
        }
    }
    true
}
fn destination_matches(a: &str, b: &str, kind: DestinationType) -> bool {
    if kind == DestinationType::Bolt11 {
        return a.eq_ignore_ascii_case(b);
    }
    let parse = |s: &str| {
        s.parse::<Address<coincube_core::miniscript::bitcoin::address::NetworkUnchecked>>()
            .ok()
            .map(|a| a.assume_checked().script_pubkey())
    };
    matches!((parse(a), parse(b)), (Some(a), Some(b)) if a == b)
}
fn bounded_text(s: &str, max: usize) -> String {
    s.chars()
        .filter(|c| {
            !c.is_control() && !matches!(*c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .take(max)
        .collect()
}
fn same_origin(network: Network, value: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(value) else {
        return false;
    };
    let base = reqwest::Url::parse(endpoint(network).url()).expect("fixed HTTPS endpoint");
    url.scheme() == "https"
        && url.origin() == base.origin()
        && url.username().is_empty()
        && url.password().is_none()
}
type LogoCache = Mutex<VecDeque<(String, Handle)>>;
static LOGOS: OnceLock<LogoCache> = OnceLock::new();
static LOGO_DECODER: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

fn logo_request_url(network: Network, value: &str) -> Option<reqwest::Url> {
    if !same_origin(network, value) {
        return None;
    }
    let mut url = reqwest::Url::parse(value).ok()?;
    // Branta's Active Storage blob URLs redirect to object storage. Its proxy
    // route serves the same signed blob without sending the client off-origin.
    if let Some(blob) = url
        .path()
        .strip_prefix("/rails/active_storage/blobs/redirect/")
    {
        let path = format!("/rails/active_storage/blobs/proxy/{blob}");
        url.set_path(&path);
    }
    Some(url)
}

async fn fetch_logo(network: Network, url: &str) -> Option<Handle> {
    let request_url = logo_request_url(network, url)?;
    let cache = LOGOS.get_or_init(|| Mutex::new(VecDeque::new()));
    if let Some((_, handle)) = cache.lock().ok()?.iter().find(|(key, _)| key == url) {
        return Some(handle.clone());
    }
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(TIMEOUT)
        .build()
        .ok()?;
    let mut response = client.get(request_url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)?
        .to_str()
        .ok()?
        .split(';')
        .next()?;
    let format = match content_type {
        "image/png" => image::ImageFormat::Png,
        "image/jpeg" => image::ImageFormat::Jpeg,
        _ => return None,
    };
    if response
        .content_length()
        .is_some_and(|len| len > MAX_LOGO_BYTES as u64)
    {
        return None;
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if bytes.len() + chunk.len() > MAX_LOGO_BYTES {
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    // Large but bounded originals must not block the async runtime or decode in
    // parallel across recipients. The permit remains held if the lookup expires
    // while its blocking decode finishes; no network work runs in this closure.
    let permit = LOGO_DECODER.acquire().await.ok()?;
    let handle = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        decode_logo(&bytes, format)
    })
    .await
    .ok()??;
    let mut cache = cache.lock().ok()?;
    if cache.len() >= 32 {
        cache.pop_front();
    }
    cache.push_back((url.to_string(), handle.clone()));
    Some(handle)
}
fn decode_logo(bytes: &[u8], format: image::ImageFormat) -> Option<Handle> {
    if bytes.len() > MAX_LOGO_BYTES || image::guess_format(bytes).ok()? != format {
        return None;
    }
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_LOGO_DIMENSION);
    limits.max_image_height = Some(MAX_LOGO_DIMENSION);
    limits.max_alloc = Some(100 * 1024 * 1024);
    reader.limits(limits);
    let image = reader
        .decode()
        .ok()?
        .thumbnail(LOGO_THUMBNAIL_DIMENSION, LOGO_THUMBNAIL_DIMENSION)
        .into_rgba8();
    Some(Handle::from_rgba(
        image.width(),
        image.height(),
        image.into_raw(),
    ))
}

#[cfg(test)]
pub fn test_ticket(enabled: bool) -> LookupTicket {
    LookupTicket {
        privacy: Arc::new(Privacy::new(enabled)),
        epoch: 0,
        network: Network::Bitcoin,
        backend: Arc::new(tests::Fake::default()),
    }
}
#[cfg(test)]
pub fn test_identity(ticket: &LookupTicket, platform: &str) -> RecipientIdentity {
    RecipientIdentity {
        platform: platform.into(),
        description: None,
        logo: None,
        logo_light: None,
        verify_url: "https://guardrail.branta.pro/v2/verify/test#secret-not-for-logs".into(),
        privacy: ticket.privacy.clone(),
        epoch: ticket.epoch,
    }
}
#[cfg(test)]
impl LookupTicket {
    pub fn set_test_enabled(&self, enabled: bool) {
        self.privacy.set(enabled);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[derive(Default)]
    pub(super) struct Fake {
        calls: AtomicUsize,
        logos: AtomicUsize,
        mode: u8,
    }
    #[async_trait]
    impl LookupBackend for Fake {
        async fn lookup(
            &self,
            _: Network,
            request: &LookupRequest,
        ) -> Result<branta::PaymentsResult, BrantaError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match self.mode {
                1 => return Err(BrantaError::Tampered),
                2 => return Err(BrantaError::RequestFailed { status: 503 }),
                3 => tokio::time::sleep(Duration::from_secs(10)).await,
                4 => return Ok(branta::PaymentsResult::default()),
                _ => (),
            }
            let mut dest = branta::Destination::new(&request.destination, Some(request.kind));
            dest.is_zk = true;
            if self.mode == 5 {
                dest.is_encrypted = true;
            }
            if self.mode == 6 {
                dest.value = "lnbc-other".into();
            }
            Ok(branta::PaymentsResult {
                payments: vec![branta::Payment {
                    platform: Some("Merchant".into()),
                    description: Some("An order".into()),
                    destinations: vec![dest],
                    platform_logo_url: Some("https://guardrail.branta.pro/logo.png".into()),
                    platform_logo_light_url: (matches!(self.mode, 7 | 9 | 10))
                        .then(|| "https://guardrail.branta.pro/light.png".into()),
                    ..Default::default()
                }],
                verify_url: "https://guardrail.branta.pro/v2/verify/example#secret".into(),
            })
        }
        async fn logo(&self, _: Network, url: &str) -> Option<Handle> {
            self.logos.fetch_add(1, Ordering::SeqCst);
            if self.mode == 7 {
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
            if self.mode == 9 || self.mode == 10 {
                let light = url.ends_with("light.png");
                if light == (self.mode == 9) {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
                return Some(Handle::from_rgba(1, 1, vec![255; 4]));
            }
            if self.mode == 8 {
                tokio::time::sleep(Duration::from_millis(500)).await;
                return Some(Handle::from_rgba(1, 1, vec![255; 4]));
            }
            None
        }
    }
    fn fake_ticket(mode: u8) -> (LookupTicket, Arc<Fake>) {
        let fake = Arc::new(Fake {
            mode,
            ..Default::default()
        });
        (
            ticket(Arc::new(Privacy::new(true)), Network::Bitcoin, fake.clone()).unwrap(),
            fake,
        )
    }
    fn request() -> LookupRequest {
        LookupRequest::bolt11("lnbc1test").unwrap()
    }
    #[test]
    fn endpoints_and_unsupported_inputs() {
        assert_eq!(endpoint(Network::Bitcoin), BrantaServerBaseUrl::Production);
        for n in [
            Network::Testnet,
            Network::Testnet4,
            Network::Signet,
            Network::Regtest,
        ] {
            assert_eq!(endpoint(n), BrantaServerBaseUrl::Staging);
        }
        for raw in [
            "lno1offer",
            "user@example.org",
            "spark1address",
            "sp1address",
            "ark1address",
            "liquidnetwork:abc",
            "😀x",
            "lightning:lnbc1?branta_secret=x",
        ] {
            assert!(LookupRequest::bolt11(raw).is_none());
        }
        assert!(LookupRequest::bolt11("LIGHTNING:LNBC1TEST").is_some());
    }
    #[tokio::test]
    async fn positive_bolt11_and_logo_failure_are_nonblocking() {
        let (t, fake) = fake_ticket(0);
        let LookupResult::Identified(ids) = t.lookup(request()).await else {
            panic!("expected identity");
        };
        assert_eq!(ids[0].platform, "Merchant");
        assert!(ids[0].logo.is_none());
        assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
        assert_eq!(fake.logos.load(Ordering::SeqCst), 1);
        assert!(!format!("{:?}", ids[0]).contains("secret"));
    }
    #[tokio::test]
    async fn logo_can_use_remaining_lookup_budget_and_disable_cancels_it() {
        let (t, _) = fake_ticket(8);
        let LookupResult::Identified(ids) = t.lookup(request()).await else {
            panic!("expected identity with a logo");
        };
        assert!(ids[0].logo.is_some());

        let (t, fake) = fake_ticket(7);
        let other = t.clone();
        let future = tokio::spawn(async move { other.lookup(request()).await });
        while fake.logos.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        t.set_test_enabled(false);
        assert!(matches!(
            tokio::time::timeout(Duration::from_millis(100), future)
                .await
                .unwrap()
                .unwrap(),
            LookupResult::Silent
        ));
    }
    #[tokio::test]
    async fn logo_deadline_preserves_identity_and_same_preference_does_not_cancel() {
        let (t, fake) = fake_ticket(7);
        let other = t.clone();
        let future = tokio::spawn(async move {
            other
                .lookup_inner(
                    request(),
                    tokio::time::Instant::now() + Duration::from_millis(50),
                )
                .await
        });
        while fake.logos.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
        let mut changes = t.privacy.state.subscribe();
        t.set_test_enabled(true);
        assert!(!changes.has_changed().unwrap());
        let LookupResult::Identified(ids) = future.await.unwrap() else {
            panic!("logo deadline discarded identity")
        };
        assert_eq!(ids[0].platform, "Merchant");
        assert!(ids[0].logo.is_none() && ids[0].logo_light.is_none());
        t.set_test_enabled(false);
        assert!(changes.changed().await.is_ok());
        assert!(!ids[0].is_current());
    }
    #[tokio::test]
    async fn completed_logo_variant_survives_other_variant_timeout() {
        for mode in [9, 10] {
            let (t, _) = fake_ticket(mode);
            let LookupResult::Identified(ids) = t
                .lookup_inner(
                    request(),
                    tokio::time::Instant::now() + Duration::from_millis(100),
                )
                .await
            else {
                panic!("expected identity")
            };
            assert_eq!(ids[0].logo.is_some(), mode == 9);
            assert_eq!(ids[0].logo_light.is_some(), mode == 10);
        }
    }
    #[tokio::test]
    async fn stalled_logos_do_not_starve_later_recipient_lookups() {
        let (t, fake) = fake_ticket(7);
        let results = t
            .lookup_many((0..4).map(|id| (id, request())).collect())
            .await;
        assert_eq!(fake.calls.load(Ordering::SeqCst), 4);
        assert_eq!(results.len(), 4);
        assert!(results
            .iter()
            .all(|(_, result)| matches!(result, LookupResult::Identified(_))));
    }
    #[tokio::test]
    async fn disabled_makes_zero_lookup_and_logo_requests_and_invalidates_off_on() {
        let (t, fake) = fake_ticket(0);
        t.set_test_enabled(false);
        assert!(matches!(t.lookup(request()).await, LookupResult::Silent));
        assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
        assert_eq!(fake.logos.load(Ordering::SeqCst), 0);
        t.set_test_enabled(true);
        assert!(!t.is_current());
        assert!(matches!(t.lookup(request()).await, LookupResult::Silent));
    }
    #[tokio::test]
    async fn disabling_cancels_inflight_without_logo_request() {
        let (t, fake) = fake_ticket(3);
        let other = t.clone();
        let future = tokio::spawn(async move { other.lookup(request()).await });
        while fake.calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        t.set_test_enabled(false);
        assert!(matches!(
            tokio::time::timeout(Duration::from_millis(100), future)
                .await
                .unwrap()
                .unwrap(),
            LookupResult::Silent
        ));
        assert_eq!(fake.logos.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn ordinary_errors_empty_unbound_and_timeout_are_silent_only_tampered_blocks() {
        for mode in [2, 3, 4, 5, 6] {
            let (t, _) = fake_ticket(mode);
            assert!(matches!(t.lookup(request()).await, LookupResult::Silent));
        }
        let (t, _) = fake_ticket(1);
        assert!(matches!(t.lookup(request()).await, LookupResult::Tampered));
    }
    #[tokio::test]
    async fn duplicate_requests_preserve_row_ids() {
        let (t, fake) = fake_ticket(0);
        let mut results = t.lookup_many(vec![(7, request()), (2, request())]).await;
        results.sort_by_key(|r| r.0);
        assert_eq!(results.iter().map(|r| r.0).collect::<Vec<_>>(), vec![2, 7]);
        assert_eq!(fake.calls.load(Ordering::SeqCst), 2);
    }
    #[test]
    fn logo_origins_decode_and_text_limits() {
        for url in [
            "http://guardrail.branta.pro/a",
            "https://evil.test/a",
            "https://guardrail.branta.pro.evil.test/a",
            "https://user@guardrail.branta.pro/a",
        ] {
            assert!(!same_origin(Network::Bitcoin, url));
        }
        assert!(!same_origin(
            Network::Regtest,
            "https://guardrail.branta.pro/a"
        ));
        assert!(decode_logo(b"not an image", image::ImageFormat::Png).is_none());
        assert_eq!(bounded_text("a\n\u{202e}b", 2), "ab");
        assert_eq!(bounded_text(&"z".repeat(300), 200).len(), 200);
    }
    #[test]
    fn active_storage_logos_use_same_origin_proxy_only() {
        let input = "https://guardrail.branta.pro/rails/active_storage/blobs/redirect/signed/logo%20dark.png?x=1";
        assert_eq!(
            logo_request_url(Network::Bitcoin, input).unwrap().as_str(),
            "https://guardrail.branta.pro/rails/active_storage/blobs/proxy/signed/logo%20dark.png?x=1"
        );
        assert!(logo_request_url(Network::Regtest, input).is_none());
        assert!(
            logo_request_url(Network::Bitcoin, "https://object-storage.test/logo.png").is_none()
        );
        let staging = input.replace("guardrail.branta.pro", "staging.guardrail.branta.pro");
        assert_eq!(
            logo_request_url(Network::Regtest, &staging)
                .unwrap()
                .host_str(),
            Some("staging.guardrail.branta.pro")
        );
        let ordinary = "https://guardrail.branta.pro/logo.png";
        assert_eq!(
            logo_request_url(Network::Bitcoin, ordinary)
                .unwrap()
                .as_str(),
            ordinary
        );
    }

    #[test]
    fn large_vendor_logo_becomes_bounded_thumbnail_and_oversize_is_refused() {
        fn png(width: u32, height: u32) -> Vec<u8> {
            let mut bytes = Cursor::new(Vec::new());
            use image::ImageEncoder;
            let image = image::RgbaImage::new(width, height);
            image::codecs::png::PngEncoder::new_with_quality(
                &mut bytes,
                image::codecs::png::CompressionType::Best,
                image::codecs::png::FilterType::NoFilter,
            )
            .write_image(
                image.as_raw(),
                width,
                height,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
            bytes.into_inner()
        }
        let bytes = png(4501, 4500);
        assert!(bytes.len() <= MAX_LOGO_BYTES);
        let handle = decode_logo(&bytes, image::ImageFormat::Png).unwrap();
        let Handle::Rgba {
            width,
            height,
            pixels,
            ..
        } = handle
        else {
            panic!("expected decoded thumbnail")
        };
        assert!((1..=64).contains(&width) && (1..=64).contains(&height));
        assert_eq!(pixels.len(), (width * height * 4) as usize);
        assert!(decode_logo(&bytes, image::ImageFormat::Jpeg).is_none());
        assert!(decode_logo(&png(5001, 1), image::ImageFormat::Png).is_none());
        assert!(decode_logo(&vec![0; MAX_LOGO_BYTES + 1], image::ImageFormat::Png).is_none());
    }
}

#[cfg(test)]
mod sdk_contract_tests {
    use super::*;
    struct Client {
        payment: branta::Payment,
        lookups: Arc<Mutex<Vec<String>>>,
    }
    #[async_trait]
    impl branta::BrantaClientTrait for Client {
        async fn get_payments<'a>(
            &'a self,
            value: &'a str,
            _: Option<&'a BrantaClientOptions>,
        ) -> Result<Vec<branta::Payment>, BrantaError> {
            self.lookups.lock().unwrap().push(value.to_string());
            Ok(vec![self.payment.clone()])
        }
        async fn post_payment<'a>(
            &'a self,
            _: branta::Payment,
            _: Option<&'a BrantaClientOptions>,
        ) -> Result<Option<branta::Payment>, BrantaError> {
            panic!("send-only")
        }
        async fn is_api_key_valid<'a>(
            &'a self,
            _: Option<&'a BrantaClientOptions>,
        ) -> Result<bool, BrantaError> {
            panic!("no API keys")
        }
    }
    fn service(
        value: &str,
        secret: &str,
        kind: DestinationType,
    ) -> (BrantaService, Arc<Mutex<Vec<String>>>) {
        let lookups = Arc::new(Mutex::new(Vec::new()));
        let mut destination = branta::Destination::new(
            branta::v2::encrypt(value, secret, true).unwrap(),
            Some(kind),
        );
        destination.is_zk = true;
        let client = Client {
            payment: branta::Payment {
                platform: Some("Fixture".into()),
                destinations: vec![destination],
                ..Default::default()
            },
            lookups: lookups.clone(),
        };
        (
            BrantaService::with_deps(
                BrantaClientOptions {
                    base_url: BrantaServerBaseUrl::Staging,
                    privacy: PrivacyMode::Strict,
                    default_api_key: None,
                    hmac_secret: None,
                },
                Box::new(client),
                Box::new(branta::v2::AesEncryptionService),
                Box::new(branta::GuidSecretGenerator),
            ),
            lookups,
        )
    }
    const ADDRESS: &str = "bc1qvrl2849aggm6qry9ea7xqp2kk39j8vaa8r3cwg";
    #[tokio::test]
    async fn official_sdk_distinguishes_authenticated_swap_from_wrong_secret() {
        let (sdk, _) = service(ADDRESS, "key", DestinationType::BitcoinAddress);
        let changed = "bc1qxy2kgdygjrsqtzq2n0yrf2493p83kkfjhx0wlh";
        let uri = format!("bitcoin:{changed}?branta_id=opaque&branta_secret=key");
        assert!(matches!(
            sdk.get_payments_by_qr_code(&uri, None).await,
            Err(BrantaError::Tampered)
        ));
        let wrong_key = uri.replace("branta_secret=key", "branta_secret=wrong");
        let result = sdk.get_payments_by_qr_code(&wrong_key, None).await.unwrap();
        assert!(result.payments[0].destinations[0].is_encrypted);
    }
    #[tokio::test]
    async fn official_strict_bolt11_never_sends_plaintext_to_http_seam() {
        let invoice = "lnbc1fixture";
        let key = branta::extensions::to_normalized_hash(invoice);
        let (sdk, lookups) = service(invoice, &key, DestinationType::Bolt11);
        let result = sdk.get_payments_by_qr_code(invoice, None).await.unwrap();
        assert_eq!(result.payments[0].destinations[0].value, invoice);
        assert!(!result.payments[0].destinations[0].is_encrypted);
        assert_ne!(lookups.lock().unwrap()[0], invoice);
        let count = lookups.lock().unwrap().len();
        assert!(sdk
            .get_payments_by_qr_code(ADDRESS, None)
            .await
            .unwrap()
            .payments
            .is_empty());
        assert_eq!(lookups.lock().unwrap().len(), count);
    }
    #[test]
    fn request_preserves_original_bip21_and_rejects_ambiguous_or_plain_id() {
        let address = ADDRESS
            .parse::<Address<coincube_core::miniscript::bitcoin::address::NetworkUnchecked>>()
            .unwrap()
            .require_network(Network::Bitcoin)
            .unwrap();
        let encrypted = branta::v2::encrypt(ADDRESS, "key", true).unwrap();
        let raw = format!("bitcoin:{ADDRESS}?amount=0.001&branta_id={encrypted}&branta_secret=key");
        let request = LookupRequest::bitcoin(&raw, &address).unwrap();
        assert_eq!(request.raw, raw);
        assert!(!format!("{request:?}").contains("key"));
        assert!(LookupRequest::bitcoin(&format!("{raw}&branta_id=duplicate"), &address).is_none());
        assert!(LookupRequest::bitcoin(
            &format!("bitcoin:{ADDRESS}?branta_id={ADDRESS}&branta_secret=key"),
            &address
        )
        .is_none());
        assert!(
            LookupRequest::bitcoin(&format!("{raw}&lightning=%F0%9F%98%80"), &address).is_none()
        );
        assert!(!safe_percent_encoding("x%z0"));
    }
}

#[cfg(test)]
mod vendor_integrity {
    #[test]
    fn vendored_sdk_matches_reviewed_source_and_manifest_adjustment() {
        use sha2::{Digest, Sha256};
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../vendor/branta");
        let hashes: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("UPSTREAM_SHA256.json")).unwrap())
                .unwrap();
        assert_eq!(hashes["commit"], "ba5e510a3189f32d11cb57e10267284b7109cb39");
        for (path, expected) in hashes["files"].as_object().unwrap() {
            let actual = hex::encode(Sha256::digest(std::fs::read(root.join(path)).unwrap()));
            assert_eq!(
                actual,
                expected.as_str().unwrap(),
                "unreviewed SDK change: {path}"
            );
        }
        let manifest = hex::encode(Sha256::digest(
            std::fs::read(root.join("Cargo.toml")).unwrap(),
        ));
        assert_eq!(
            manifest,
            hashes["adapted_manifest_sha256"].as_str().unwrap()
        );
    }
}
