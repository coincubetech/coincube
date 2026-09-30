//! Single-attempt node HTTP transport, used only after a Claim gate is entered.
use crate::config::{BitcoindConfig, BitcoindRpcAuth};
use miniscript::bitcoin::{consensus::encode::serialize_hex, Transaction, Txid};
use serde::Deserialize;
use std::{fs::File, io::Read, time::Duration};

const MAX_REPLY: usize = 16_384;

pub(super) struct PreparedNode {
    client: reqwest::blocking::Client,
    request: reqwest::blocking::Request,
    txid: Txid,
}
impl PreparedNode {
    /// Resolve credentials and construct the request before taking the backend
    /// lock or consuming a gate. The client has no automatic resend paths.
    pub(super) fn new(config: &BitcoindConfig, tx: &Transaction) -> Result<Self, ()> {
        let (user, password) = match &config.rpc_auth {
            BitcoindRpcAuth::UserPass(user, password) => (user.clone(), password.clone()),
            BitcoindRpcAuth::CookieFile(path) => {
                let mut bytes = Vec::new();
                File::open(path)
                    .map_err(|_| ())?
                    .take(16_385)
                    .read_to_end(&mut bytes)
                    .map_err(|_| ())?;
                if bytes.len() > 16_384 {
                    return Err(());
                }
                let cookie = std::str::from_utf8(&bytes).map_err(|_| ())?;
                let (user, password) = cookie
                    .trim_end_matches(['\r', '\n'])
                    .split_once(':')
                    .ok_or(())?;
                if user.is_empty() || password.is_empty() {
                    return Err(());
                }
                (user.to_owned(), password.to_owned())
            }
        };
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .http1_only()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|_| ())?;
        let request = client.post(format!("http://{}/", config.addr))
            .basic_auth(user, Some(password))
            .json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"sendrawtransaction","params":[serialize_hex(tx)]}))
            .build().map_err(|_| ())?;
        Ok(Self {
            client,
            request,
            txid: tx.compute_txid(),
        })
    }

    /// Consume one prepared request. Any error is ambiguous after gate entry;
    /// no cookie refresh, redirect, fallback, or transport retry is attempted.
    pub(super) fn send(self) -> Result<(), ()> {
        let response = self.client.execute(self.request).map_err(|_| ())?;
        if response.status() != reqwest::StatusCode::OK
            || response
                .content_length()
                .is_some_and(|len| len > MAX_REPLY as u64)
        {
            return Err(());
        }
        let mut bytes = Vec::new();
        response
            .take((MAX_REPLY + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| ())?;
        if bytes.len() > MAX_REPLY {
            return Err(());
        }
        #[derive(Deserialize)]
        struct Reply {
            id: u64,
            result: Option<Txid>,
            error: Option<serde_json::Value>,
        }
        let reply: Reply = serde_json::from_slice(&bytes).map_err(|_| ())?;
        if reply.id != 1 || reply.error.is_some() || reply.result != Some(self.txid) {
            return Err(());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use miniscript::bitcoin::{absolute, transaction};

    #[test]
    fn cookie_preparation_is_bounded_and_freezes_credentials() {
        let directory = std::env::temp_dir().join(format!(
            "coincube-claim-cookie-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(directory.clone());
        let cookie = directory.join(".cookie");
        let config = BitcoindConfig {
            addr: "127.0.0.1:1".parse().unwrap(),
            rpc_auth: BitcoindRpcAuth::CookieFile(cookie.clone()),
        };
        let tx = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![],
            output: vec![],
        };
        assert!(PreparedNode::new(&config, &tx).is_err());
        for invalid in [
            vec![],
            b"missing-separator".to_vec(),
            b":password".to_vec(),
            b"user:".to_vec(),
            vec![0xff],
            [b"u:".as_slice(), &vec![b'p'; 16_383]].concat(),
        ] {
            std::fs::write(&cookie, invalid).unwrap();
            assert!(PreparedNode::new(&config, &tx).is_err());
        }
        // Colons belong to the password after the first separator; CRLF is
        // permitted for a cookie written by a Windows node.
        std::fs::write(&cookie, b"user:pass:word\r\n").unwrap();
        let prepared = PreparedNode::new(&config, &tx).unwrap();
        std::fs::write(&cookie, b"replacement:credentials").unwrap();
        assert_eq!(
            prepared.request.headers()[reqwest::header::AUTHORIZATION],
            "Basic dXNlcjpwYXNzOndvcmQ="
        );
        // The maximum accepted cookie remains bounded, including its separator.
        std::fs::write(&cookie, [b"u:".as_slice(), &vec![b'p'; 16_382]].concat()).unwrap();
        assert!(PreparedNode::new(&config, &tx).is_ok());
    }
}
