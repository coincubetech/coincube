//! Authenticated Connect observations, not chain authentication or split permission.
//!
//! A typed unavailable observation (HTTP 503) is retained. Missing schedules,
//! failed RPCs and malformed responses never become an inactive/expired schedule.

use coincube_core::chain::ChainId;
use serde::Deserialize;

use super::{CoincubeClient, CoincubeError};
use crate::services::http::ResponseExt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkStatusState {
    NotConfigured,
    ConfigurationError,
    RpcUnavailable,
    Malformed,
    ForkAbsent,
    ForkInactive,
    RdtsAbsent,
    RdtsUnsupported,
    Available,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct NetworkStatus {
    pub network: ChainId,
    pub state: NetworkStatusState,
    pub observation: Option<NetworkObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct NetworkObservation {
    pub tip_height: u64,
    #[serde(deserialize_with = "required_nullable")]
    pub fork: Option<ForkActivation>,
    pub rdts: RdtsStatus,
}

// `fork: null` means absent; an omitted field is a malformed contract response.
fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ForkActivation {
    pub height: u64,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RdtsStatus {
    Absent,
    Unsupported,
    Flagday { flagday: RdtsFlagday },
}

/// Values reported by the node for the next block, with no local-clock inference.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RdtsFlagday {
    pub height: u64,
    pub expiry_time: i64,
    pub active: bool,
}

#[derive(Debug)]
pub enum NetworkStatusError {
    /// Includes auth/feature gate/rate limit errors, preserving the HTTP status.
    Request(CoincubeError),
    UnsupportedChain,
    /// Invalid JSON, missing fields, identity mismatch or contradictory states.
    InvalidResponse,
}

impl std::fmt::Display for NetworkStatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Request(error) => write!(f, "{}", error),
            Self::UnsupportedChain => {
                write!(f, "Network status is only available for Bitcoin Blake2b")
            }
            Self::InvalidResponse => write!(f, "Connect returned an invalid network status"),
        }
    }
}

impl std::error::Error for NetworkStatusError {}

impl From<CoincubeError> for NetworkStatusError {
    fn from(error: CoincubeError) -> Self {
        Self::Request(error)
    }
}

#[derive(Deserialize)]
struct StatusEnvelope {
    success: bool,
    data: NetworkStatus,
    error: Option<StatusErrorBody>,
}

#[derive(Deserialize)]
struct StatusErrorBody {
    code: String,
}

impl NetworkStatus {
    fn consistent(&self) -> bool {
        use NetworkStatusState::*;
        let Some(observation) = &self.observation else {
            return matches!(
                self.state,
                NotConfigured | ConfigurationError | RpcUnavailable | Malformed
            );
        };
        let expected = match &observation.fork {
            None => ForkAbsent,
            // `blake2b.active` is `DeploymentActiveAfter(tip, DEPLOYMENT_BLAKE2B)`:
            // it reports whether the rules apply to the block AFTER `tip_height`.
            // A single `getdeploymentinfo` response supplies both fields, so they
            // describe one node snapshot and cannot race a block apart. An active
            // fork therefore cannot activate later than `tip_height + 1`; a claim
            // that it does is contradictory, not merely pending.
            //
            // `height == tip_height + 1` is the activating block and stays valid.
            //
            // The converse (`!active` with `height <= tip_height + 1`) is equally
            // impossible, but is deliberately NOT rejected here: it resolves to
            // `ForkInactive`, which already refuses the schedule, so rejecting it
            // as malformed would trade one refusal for another while making a
            // benign server bug fatal. Only the direction that could present an
            // unactivated fork as usable is policed.
            Some(fork) if fork.active && fork.height > observation.tip_height.saturating_add(1) => {
                return false;
            }
            Some(fork) if !fork.active => ForkInactive,
            Some(_) => match observation.rdts {
                RdtsStatus::Absent => RdtsAbsent,
                RdtsStatus::Unsupported => RdtsUnsupported,
                RdtsStatus::Flagday { .. } => Available,
            },
        };
        self.state == expected
    }
}

impl CoincubeClient {
    /// Fetch one fresh observation through the existing authenticated client.
    ///
    /// `Ok` includes typed unavailable states; callers must inspect `state`.
    /// Even `Available` is only the configured node's claim. Expiry margins,
    /// reorg/depth checks and positive poison evidence remain separate gates.
    pub async fn network_status(
        &self,
        chain: ChainId,
    ) -> Result<NetworkStatus, NetworkStatusError> {
        if !chain.is_blake2b() {
            return Err(NetworkStatusError::UnsupportedChain);
        }
        let response = self
            .client
            .get(format!(
                "{}/api/v1/connect/networks/{}/status",
                self.base_url,
                chain.api_str()
            ))
            .send()
            .await
            .map_err(CoincubeError::from)?;
        let status = response.status();
        if status != reqwest::StatusCode::OK && status != reqwest::StatusCode::SERVICE_UNAVAILABLE {
            response
                .check_success()
                .await
                .map_err(CoincubeError::from)?;
            return Err(NetworkStatusError::InvalidResponse);
        }
        let envelope: StatusEnvelope = response
            .json()
            .await
            .map_err(|_| NetworkStatusError::InvalidResponse)?;
        let available = envelope.data.state == NetworkStatusState::Available;
        let valid_error = if available {
            envelope.error.is_none()
        } else {
            envelope
                .error
                .is_some_and(|error| error.code == "SERVICE_UNAVAILABLE")
        };
        if envelope.data.network != chain
            || !envelope.data.consistent()
            || envelope.success != available
            || status.is_success() != available
            || !valid_error
        {
            return Err(NetworkStatusError::InvalidResponse);
        }
        Ok(envelope.data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::{Method::GET, MockServer};
    use serde_json::{json, Value};

    fn observation() -> Value {
        json!({"tip_height": 973029, "fork": {"height": 961640, "active": true},
            "rdts": {"state": "flagday", "flagday": {
                "height": 961641, "expiry_time": 1819756801_i64, "active": false}}})
    }

    fn envelope(network: &str, state: &str, observation: Option<Value>) -> Value {
        let available = state == "available";
        let mut data = json!({"network": network, "state": state});
        if let Some(observation) = observation {
            data["observation"] = observation;
        }
        json!({"success": available, "data": data, "error": if available {
            Value::Null
        } else { json!({"code": "SERVICE_UNAVAILABLE", "message": "Unavailable"}) }})
    }

    async fn fetch(
        chain: ChainId,
        status: u16,
        body: Value,
    ) -> Result<NetworkStatus, NetworkStatusError> {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path(format!(
                    "/api/v1/connect/networks/{}/status",
                    chain.api_str()
                ))
                .header("authorization", "Bearer synthetic-status-token");
            then.status(status).json_body(body);
        });
        let mut client = CoincubeClient::for_test(server.base_url());
        client.set_token("synthetic-status-token");
        let result = client.network_status(chain).await;
        mock.assert();
        result
    }

    #[tokio::test]
    async fn authenticated_identity_and_expired_rdts_are_preserved() {
        for chain in [ChainId::BitcoinBlake2b, ChainId::BitcoinBlake2bTestnet4] {
            let mut body = envelope(chain.api_str(), "available", Some(observation()));
            body["data"]["future_field"] = json!(true);
            let status = fetch(chain, 200, body).await.unwrap();
            assert_eq!(status.network, chain);
            assert_eq!(status.state, NetworkStatusState::Available);
            let observed = status.observation.unwrap();
            assert_eq!(observed.tip_height, 973029);
            assert_eq!(observed.fork.unwrap().height, 961640);
            assert_eq!(
                observed.rdts,
                RdtsStatus::Flagday {
                    flagday: RdtsFlagday {
                        height: 961641,
                        expiry_time: 1819756801,
                        active: false,
                    }
                }
            );
        }
    }

    #[tokio::test]
    async fn unavailable_states_remain_distinct() {
        use NetworkStatusState::*;
        for (wire, state) in [
            ("not_configured", NotConfigured),
            ("configuration_error", ConfigurationError),
            ("rpc_unavailable", RpcUnavailable),
            ("malformed", Malformed),
        ] {
            let status = fetch(
                ChainId::BitcoinBlake2b,
                503,
                envelope("bitcoin-blake2b", wire, None),
            )
            .await
            .unwrap();
            assert_eq!(status.state, state);
            assert_eq!(status.observation, None);
        }
        for (wire, state) in [
            ("fork_absent", ForkAbsent),
            ("fork_inactive", ForkInactive),
            ("rdts_absent", RdtsAbsent),
            ("rdts_unsupported", RdtsUnsupported),
        ] {
            let mut observed = observation();
            match state {
                ForkAbsent => observed["fork"] = Value::Null,
                ForkInactive => observed["fork"]["active"] = json!(false),
                RdtsAbsent => observed["rdts"] = json!({"state": "absent"}),
                RdtsUnsupported => observed["rdts"] = json!({"state": "unsupported"}),
                _ => unreachable!(),
            }
            let status = fetch(
                ChainId::BitcoinBlake2b,
                503,
                envelope("bitcoin-blake2b", wire, Some(observed)),
            )
            .await
            .unwrap();
            assert_eq!(status.state, state);
            assert!(status.observation.is_some());
        }
    }

    #[tokio::test]
    async fn invalid_matrix_never_becomes_a_usable_schedule() {
        let valid = envelope("bitcoin-blake2b", "available", Some(observation()));
        let mut invalid = Vec::new();
        for pointer in [
            "/data/observation/tip_height",
            "/data/observation/fork/height",
            "/data/observation/fork/active",
            "/data/observation/rdts/flagday/height",
            "/data/observation/rdts/flagday/expiry_time",
            "/data/observation/rdts/flagday/active",
        ] {
            for bad in [Value::Null, json!("wrong"), json!({})] {
                let mut body = valid.clone();
                *body.pointer_mut(pointer).unwrap() = bad;
                invalid.push(body);
            }
            let mut body = valid.clone();
            let (parent, key) = pointer.rsplit_once('/').unwrap();
            body.pointer_mut(parent)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(key);
            invalid.push(body);
        }
        for (pointer, bad) in [
            ("/data/network", json!("bitcoin-blake2b-testnet4")),
            ("/data/network", json!("bitcoin")),
            ("/data/state", json!("unknown")),
            ("/data/observation", Value::Null),
            ("/data/observation/fork/active", json!(false)),
            ("/data/observation/rdts/state", json!("unknown")),
            ("/data/observation/tip_height", json!(-1)),
            ("/data/observation/rdts/flagday/expiry_time", json!(1.5)),
            ("/success", json!(false)),
        ] {
            let mut body = valid.clone();
            *body.pointer_mut(pointer).unwrap() = bad;
            invalid.push(body);
        }
        // A missing fork field is malformed, rather than a valid absent fork.
        let mut missing_fork = envelope("bitcoin-blake2b", "fork_absent", Some(observation()));
        missing_fork["data"]["observation"]
            .as_object_mut()
            .unwrap()
            .remove("fork");
        assert!(matches!(
            fetch(ChainId::BitcoinBlake2b, 503, missing_fork).await,
            Err(NetworkStatusError::InvalidResponse)
        ));
        for body in invalid {
            assert!(matches!(
                fetch(ChainId::BitcoinBlake2b, 200, body).await,
                Err(NetworkStatusError::InvalidResponse)
            ));
        }
        assert!(matches!(
            fetch(ChainId::BitcoinBlake2b, 503, valid).await,
            Err(NetworkStatusError::InvalidResponse)
        ));
        assert!(matches!(
            fetch(
                ChainId::BitcoinBlake2b,
                200,
                envelope("bitcoin-blake2b", "not_configured", None)
            )
            .await,
            Err(NetworkStatusError::InvalidResponse)
        ));
    }

    /// `blake2b.active` describes the block after `tip_height`, and both fields
    /// come from one `getdeploymentinfo` snapshot, so an active fork can activate
    /// at most one block past the tip. The activating block itself is valid.
    #[tokio::test]
    async fn active_fork_scheduled_past_the_next_block_is_refused() {
        let tip = 973029_u64;
        for height in [0, 1, tip - 1, tip, tip + 1] {
            let mut observed = observation();
            observed["tip_height"] = json!(tip);
            observed["fork"]["height"] = json!(height);
            let status = fetch(
                ChainId::BitcoinBlake2b,
                200,
                envelope("bitcoin-blake2b", "available", Some(observed)),
            )
            .await
            .unwrap();
            assert_eq!(
                status.state,
                NetworkStatusState::Available,
                "active fork at {height} with tip {tip} must stay valid"
            );
        }
        // One past the activating block, and a value that would overflow a
        // non-saturating add, are both contradictory rather than pending.
        for height in [tip + 2, tip + 10_000, u64::MAX] {
            let mut observed = observation();
            observed["tip_height"] = json!(tip);
            observed["fork"]["height"] = json!(height);
            assert!(
                matches!(
                    fetch(
                        ChainId::BitcoinBlake2b,
                        200,
                        envelope("bitcoin-blake2b", "available", Some(observed)),
                    )
                    .await,
                    Err(NetworkStatusError::InvalidResponse)
                ),
                "active fork at {} with tip {} must be refused",
                height,
                tip
            );
        }
        // The converse is impossible too, but is deliberately still accepted:
        // it resolves to `ForkInactive`, which already refuses the schedule.
        // This pins that asymmetry so it is not "completed" by accident.
        let mut inactive = observation();
        inactive["tip_height"] = json!(tip);
        inactive["fork"] = json!({"height": tip - 1, "active": false});
        let status = fetch(
            ChainId::BitcoinBlake2b,
            503,
            envelope("bitcoin-blake2b", "fork_inactive", Some(inactive)),
        )
        .await
        .unwrap();
        assert_eq!(status.state, NetworkStatusState::ForkInactive);
    }

    #[tokio::test]
    async fn gates_and_upstream_http_failures_are_not_observations() {
        for code in [401, 404, 429, 500] {
            let result = fetch(
                ChainId::BitcoinBlake2b,
                code,
                json!({"success": false, "data": null}),
            )
            .await;
            match result {
                Err(NetworkStatusError::Request(CoincubeError::Unsuccessful(info))) => {
                    assert_eq!(info.status_code, code)
                }
                other => panic!("unexpected result: {:?}", other),
            }
        }
        assert!(matches!(
            fetch(ChainId::BitcoinBlake2b, 503, json!({"error": "proxy down"})).await,
            Err(NetworkStatusError::InvalidResponse)
        ));
    }

    #[tokio::test]
    async fn bitcoin_chains_are_refused_without_requests() {
        let client = CoincubeClient::for_test("http://127.0.0.1:1");
        for chain in ChainId::ALL
            .iter()
            .copied()
            .filter(|chain| !chain.is_blake2b())
        {
            assert!(matches!(
                client.network_status(chain).await,
                Err(NetworkStatusError::UnsupportedChain)
            ));
        }
    }
}
