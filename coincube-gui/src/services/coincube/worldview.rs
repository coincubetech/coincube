//! Account-scoped presentation snapshots. The strict wire decoder intentionally
//! does not use the device preference enums' forward-compatible fallbacks.

use crate::app::settings::preferences::{ArtworkStyle, DevicePreferences, WorldviewMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Selection {
    pub mode: WorldviewMode,
    pub artwork: ArtworkStyle,
}

impl From<DevicePreferences> for Selection {
    fn from(saved: DevicePreferences) -> Self {
        Self {
            mode: saved.worldview,
            artwork: saved.artwork.get(saved.worldview),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub account_uuid: String,
    pub revision: u64,
    pub selection: Option<Selection>,
    pub etag: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Unavailable,
    Conflict,
    Locked,
}

impl Snapshot {
    pub fn decode(body: &[u8], etag: &str) -> Result<Self, Error> {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct WireSnapshot {
            version: u32,
            account_uuid: uuid::Uuid,
            revision: u64,
            selection: serde_json::Value,
        }
        #[derive(serde::Deserialize)]
        struct Envelope {
            success: bool,
            data: WireSnapshot,
        }
        let wire: Envelope = serde_json::from_slice(body).map_err(|_| Error::Unavailable)?;
        let data = wire.data;
        let account_uuid = data.account_uuid.to_string();
        if !wire.success
            || data.version != 1
            || data.revision > i64::MAX as u64
            || !strong_etag(etag)
        {
            return Err(Error::Unavailable);
        }
        let selection = if data.selection.is_null() {
            None
        } else {
            let mode = match data.selection.get("mode").and_then(|value| value.as_str()) {
                Some("samurai") => WorldviewMode::Samurai,
                Some("suitcoiner") => WorldviewMode::Suitcoiner,
                Some("luke") => WorldviewMode::Luke,
                _ => return Err(Error::Unavailable),
            };
            let artwork = match data
                .selection
                .get("artwork")
                .and_then(|value| value.as_str())
            {
                Some("existing") => ArtworkStyle::Existing,
                Some("scene") => ArtworkStyle::Scene,
                _ => return Err(Error::Unavailable),
            };
            if data.revision == 0 {
                return Err(Error::Unavailable);
            }
            Some(Selection { mode, artwork })
        };
        Ok(Self {
            account_uuid,
            revision: data.revision,
            selection,
            etag: etag.to_owned(),
        })
    }
}

fn strong_etag(etag: &str) -> bool {
    etag.strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .is_some_and(|value| {
            value
                .bytes()
                .all(|byte| byte == 0x21 || (0x23..=0x7e).contains(&byte))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ACCOUNT: &str = "82473444-c619-4c95-bc6e-ef2d9f692e8b";

    fn decode(mode: &str, artwork: &str) -> Result<Snapshot, Error> {
        let body = json!({"success":true,"data":{
            "version":1,"accountUuid":ACCOUNT,"revision":1,
            "selection":{"mode":mode,"artwork":artwork}
        }});
        Snapshot::decode(
            &serde_json::to_vec(&body).unwrap(),
            &format!("\"worldview-v1-{ACCOUNT}-1\""),
        )
    }

    #[test]
    fn all_six_pairs_round_trip_without_unknown_enum_fallbacks() {
        for mode in ["samurai", "suitcoiner", "luke"] {
            for artwork in ["existing", "scene"] {
                let snapshot = decode(mode, artwork).unwrap();
                let actual = serde_json::to_value(snapshot.selection.unwrap()).unwrap();
                assert_eq!(actual, json!({"mode":mode,"artwork":artwork}));
            }
        }
        assert_eq!(decode("future", "scene"), Err(Error::Unavailable));
        assert_eq!(decode("luke", "future"), Err(Error::Unavailable));
    }

    #[test]
    fn unset_and_revoked_are_distinct_from_malformed_snapshots() {
        let mut data = json!({"success":true,"data":{
            "version":1,"accountUuid":ACCOUNT,"revision":0,"selection":null
        }});
        let etag = format!("\"worldview-v1-{ACCOUNT}-0\"");
        assert!(Snapshot::decode(&serde_json::to_vec(&data).unwrap(), &etag)
            .unwrap()
            .selection
            .is_none());
        data["data"].as_object_mut().unwrap().remove("selection");
        assert_eq!(
            Snapshot::decode(&serde_json::to_vec(&data).unwrap(), &etag),
            Err(Error::Unavailable)
        );
        data["data"]["selection"] = serde_json::Value::Null;
        data["data"]["version"] = json!(2);
        assert_eq!(
            Snapshot::decode(&serde_json::to_vec(&data).unwrap(), &etag),
            Err(Error::Unavailable)
        );
    }
    #[tokio::test]
    async fn conditional_publish_and_revoke_use_the_read_etag() {
        use super::super::CoincubeClient;
        use httpmock::{
            Method::{DELETE, GET, PUT},
            MockServer,
        };
        let server = MockServer::start_async().await;
        let mut client = CoincubeClient::for_test(server.base_url());
        client.set_token("worldview-fixture-token");
        let tag = |revision| format!("\"opaque-selection-token-{revision}\"");
        let body = |revision, selection| {
            json!({"success":true,"data":{
                "version":1,"accountUuid":ACCOUNT,"revision":revision,"selection":selection
            }})
        };
        let get = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/api/v1/connect/worldview")
                    .header("Authorization", "Bearer worldview-fixture-token");
                then.status(200)
                    .header("ETag", tag(0))
                    .json_body(body(0, serde_json::Value::Null));
            })
            .await;
        let initial = client.get_worldview().await.unwrap();
        get.assert_async().await;
        let selection = Selection {
            mode: WorldviewMode::Luke,
            artwork: ArtworkStyle::Scene,
        };
        let put = server
            .mock_async(|when, then| {
                when.method(PUT)
                    .path("/api/v1/connect/worldview")
                    .header("If-Match", tag(0))
                    .json_body(json!({"mode":"luke","artwork":"scene"}));
                then.status(200)
                    .header("ETag", tag(1))
                    .json_body(body(1, serde_json::to_value(selection).unwrap()));
            })
            .await;
        let shared = client
            .share_worldview(&initial, Some(selection))
            .await
            .unwrap();
        put.assert_async().await;
        let delete = server
            .mock_async(|when, then| {
                when.method(DELETE)
                    .path("/api/v1/connect/worldview")
                    .header("If-Match", tag(1));
                then.status(200)
                    .header("ETag", tag(2))
                    .json_body(body(2, serde_json::Value::Null));
            })
            .await;
        assert!(client
            .share_worldview(&shared, None)
            .await
            .unwrap()
            .selection
            .is_none());
        delete.assert_async().await;
    }

    #[tokio::test]
    async fn conflict_is_returned_without_automatic_retry() {
        use super::super::CoincubeClient;
        use httpmock::{Method::PUT, MockServer};
        let server = MockServer::start_async().await;
        let mut client = CoincubeClient::for_test(server.base_url());
        client.set_token("worldview-fixture-token");
        let current = decode("luke", "scene").unwrap();
        let put = server
            .mock_async(|when, then| {
                when.method(PUT)
                    .path("/api/v1/connect/worldview")
                    .header("Authorization", "Bearer worldview-fixture-token");
                then.status(412);
            })
            .await;
        assert_eq!(
            client.share_worldview(&current, current.selection).await,
            Err(Error::Conflict)
        );
        put.assert_hits_async(1).await;
    }
    #[test]
    fn etag_is_an_opaque_strong_validator() {
        let body = serde_json::to_vec(&json!({"success":true,"data":{
            "version":1,"accountUuid":ACCOUNT,"revision":0,"selection":null
        }}))
        .unwrap();
        let parsed = Snapshot::decode(&body, "\"opaque-token\"").unwrap();
        assert_eq!(parsed.etag, "\"opaque-token\"");
        for tag in [
            "",
            "unquoted",
            "W/\"weak\"",
            "\"bad space\"",
            "\"bad\"quote\"",
        ] {
            assert_eq!(Snapshot::decode(&body, tag), Err(Error::Unavailable));
        }
    }
}
