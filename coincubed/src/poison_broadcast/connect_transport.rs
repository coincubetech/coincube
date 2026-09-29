//! Fixed Bitcoin Connect endpoint with no automatic retry or fallback.
use miniscript::bitcoin::{consensus::encode::serialize_hex, Transaction, Txid};
use std::{io::Read, str::FromStr, time::Duration};

pub(super) struct PreparedConnect {
    client: reqwest::blocking::Client,
    request: reqwest::blocking::Request,
    txid: Txid,
}
impl PreparedConnect {
    pub(super) fn new(origin: &str, tx: &Transaction) -> Result<Self, ()> {
        let origin = reqwest::Url::parse(origin).map_err(|_| ())?;
        if !matches!(origin.scheme(), "https" | "http")
            || origin.host_str().is_none()
            || origin.path() != "/"
            || origin.query().is_some()
            || origin.fragment().is_some()
            || !origin.username().is_empty()
            || origin.password().is_some()
        {
            return Err(());
        }
        let endpoint = origin
            .join("api/v1/esplora/bitcoin/mainnet/tx")
            .map_err(|_| ())?;
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .http1_only()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|_| ())?;
        let request = client
            .post(endpoint)
            .header(reqwest::header::CONTENT_TYPE, "text/plain")
            .body(serialize_hex(tx))
            .build()
            .map_err(|_| ())?;
        Ok(Self {
            client,
            request,
            txid: tx.compute_txid(),
        })
    }

    pub(super) fn send(self) -> Result<(), ()> {
        let response = self.client.execute(self.request).map_err(|_| ())?;
        // Esplora acknowledges with the exact plain-text transaction id.
        if response.status() != reqwest::StatusCode::OK
            || response.content_length().is_some_and(|length| length > 128)
        {
            return Err(());
        }
        let mut bytes = Vec::new();
        response.take(129).read_to_end(&mut bytes).map_err(|_| ())?;
        if bytes.len() > 128 {
            return Err(());
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| ())?;
        if Txid::from_str(text.trim()).map_err(|_| ())? != self.txid {
            return Err(());
        }
        Ok(())
    }
}
