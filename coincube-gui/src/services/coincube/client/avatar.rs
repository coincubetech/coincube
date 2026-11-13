//! Avatar endpoints share their parent client's captured transport identity.
use super::CoincubeClient;
use crate::services::{
    coincube::{
        ApiResponse, AvatarGenerateData, AvatarGenerateRequest, AvatarSelectData,
        AvatarSelectRequest, CoincubeError, GetAvatarData, PublicAvatarData, RegenerationData,
    },
    http::ResponseExt,
};
use std::time::Duration;

impl CoincubeClient {
    /// GET /api/v1/connect/cubes/{cubeId}/avatar
    pub async fn get_avatar(&self, cube_id: &str) -> Result<GetAvatarData, CoincubeError> {
        let url = format!("{}/api/v1/connect/cubes/{}/avatar", self.base_url, cube_id);
        let res = self.client.get(&url).send().await?;
        let res = res.check_success().await?;
        let resp: ApiResponse<GetAvatarData> = res.json().await?;
        Ok(resp.data)
    }

    /// POST /api/v1/connect/cubes/{cubeId}/avatar/generate
    pub async fn post_avatar_generate(
        &self,
        cube_id: &str,
        req: AvatarGenerateRequest,
    ) -> Result<AvatarGenerateData, CoincubeError> {
        let slow_client = Self::client_builder(
            &self.base_url,
            &self.identity_headers,
            self.token(),
            Duration::from_secs(120),
        )?
        .build()
        .map_err(CoincubeError::Network)?;

        let url = format!(
            "{}/api/v1/connect/cubes/{}/avatar/generate",
            self.base_url, cube_id
        );
        let res = slow_client.post(&url).json(&req).send().await?;
        let res = res.check_success().await?;
        let resp: ApiResponse<AvatarGenerateData> = res.json().await?;
        Ok(resp.data)
    }

    /// POST /api/v1/connect/cubes/{cubeId}/avatar/select
    pub async fn post_avatar_select(
        &self,
        cube_id: &str,
        req: AvatarSelectRequest,
    ) -> Result<AvatarSelectData, CoincubeError> {
        let url = format!(
            "{}/api/v1/connect/cubes/{}/avatar/select",
            self.base_url, cube_id
        );
        let res = self.client.post(&url).json(&req).send().await?;
        let res = res.check_success().await?;
        let resp: ApiResponse<AvatarSelectData> = res.json().await?;
        Ok(resp.data)
    }

    /// GET /api/v1/connect/cubes/{cubeId}/avatar/regenerations
    pub async fn get_avatar_regenerations(
        &self,
        cube_id: &str,
    ) -> Result<RegenerationData, CoincubeError> {
        let url = format!(
            "{}/api/v1/connect/cubes/{}/avatar/regenerations",
            self.base_url, cube_id
        );
        let res = self.client.get(&url).send().await?;
        let res = res.check_success().await?;
        let resp: ApiResponse<RegenerationData> = res.json().await?;
        Ok(resp.data)
    }

    /// GET /api/v1/connect/avatar/public/{lightning_address} (public, no cube scope)
    pub async fn get_public_avatar(
        &self,
        lightning_address: &str,
    ) -> Result<PublicAvatarData, CoincubeError> {
        let url = format!(
            "{}/api/v1/connect/avatar/public/{}",
            self.base_url, lightning_address
        );
        let res = self.client.get(&url).send().await?;
        let res = res.check_success().await?;
        let resp: ApiResponse<PublicAvatarData> = res.json().await?;
        Ok(resp.data)
    }

    /// GET /api/v1/connect/avatar/image/{id} (public, no cube scope)
    pub async fn fetch_avatar_image(&self, variant_id: u64) -> Result<Vec<u8>, CoincubeError> {
        let url = format!(
            "{}/api/v1/connect/avatar/image/{}",
            self.base_url, variant_id
        );
        let res = self.client.get(&url).send().await?;
        let res = res.check_success().await?;
        Ok(res.bytes().await.map_err(CoincubeError::Network)?.to_vec())
    }
}

#[cfg(test)]
mod avatar_contract_tests {
    use super::*;
    use httpmock::{Method as MockMethod, MockServer};
    use serde_json::json;

    #[tokio::test]
    async fn avatar_reads_decode_envelopes_and_images_remain_raw() {
        let server = MockServer::start();
        let client = CoincubeClient::for_test(server.base_url());
        let avatar = server.mock(|when, then| {
            when.method(MockMethod::GET)
                .path("/api/v1/connect/cubes/cube-7/avatar");
            then.json_body(
                json!({"data":{"has_avatar":false,"identity":null,"regenerations_remaining":3}}),
            );
        });
        let regenerations = server.mock(|when, then| {
            when.method(MockMethod::GET)
                .path("/api/v1/connect/cubes/cube-7/avatar/regenerations");
            then.json_body(json!({"data":{"total_allowed":3,"used":1,"remaining":2}}));
        });
        let public = server.mock(|when, then| {
            when.method(MockMethod::GET).path("/api/v1/connect/avatar/public/alice@example.invalid");
            then.json_body(json!({"data":{"lightning_address":"alice@example.invalid","avatar_url":"/image/42","archetype":"ronin"}}));
        });
        let bytes = [0, 255, 137, 80, 78, 71];
        let image = server.mock(|when, then| {
            when.method(MockMethod::GET)
                .path("/api/v1/connect/avatar/image/42");
            then.body(bytes);
        });
        let result = client.get_avatar("cube-7").await.unwrap();
        assert!(!result.has_avatar);
        assert!(result.variants.is_empty());
        assert_eq!(result.regenerations_remaining, 3);
        let result = client.get_avatar_regenerations("cube-7").await.unwrap();
        assert_eq!(
            (result.total_allowed, result.used, result.remaining),
            (3, 1, 2)
        );
        let result = client
            .get_public_avatar("alice@example.invalid")
            .await
            .unwrap();
        assert_eq!(result.lightning_address, "alice@example.invalid");
        assert_eq!(result.avatar_url, "/image/42");
        assert_eq!(result.archetype, "ronin");
        assert_eq!(client.fetch_avatar_image(42).await.unwrap(), bytes);
        avatar.assert();
        regenerations.assert();
        public.assert();
        image.assert();
    }

    #[tokio::test]
    async fn avatar_posts_preserve_payloads_and_http_failures() {
        let server = MockServer::start();
        let client = CoincubeClient::for_test(server.base_url());
        let request = AvatarGenerateRequest {
            user_traits: Default::default(),
        };
        let generate = server.mock(|when, then| {
            when.method(MockMethod::POST)
                .path("/api/v1/connect/cubes/cube-7/avatar/generate")
                .json_body(serde_json::to_value(&request).unwrap());
            then.status(429)
                .json_body(json!({"error":{"message":"try later"}}));
        });
        let select = server.mock(|when, then| {
            when.method(MockMethod::POST)
                .path("/api/v1/connect/cubes/cube-7/avatar/select")
                .json_body(json!({"variant_id":42}));
            then.json_body(json!({"data":{"active_avatar_url":"/image/42","variant_id":42}}));
        });
        assert!(
            matches!(client.post_avatar_generate("cube-7", request).await, Err(CoincubeError::Unsuccessful(info)) if info.status_code == 429)
        );
        let selected = client
            .post_avatar_select("cube-7", AvatarSelectRequest { variant_id: 42 })
            .await
            .unwrap();
        assert_eq!(selected.variant_id, 42);
        assert_eq!(selected.active_avatar_url, "/image/42");
        generate.assert();
        select.assert();
    }

    #[tokio::test]
    async fn generated_identity_and_variant_decode_from_data_envelope() {
        let server = MockServer::start();
        let client = CoincubeClient::for_test(server.base_url());
        let request = AvatarGenerateRequest {
            user_traits: Default::default(),
        };
        let string_fields = |names: &str| -> serde_json::Value {
            names
                .split_whitespace()
                .map(|name| (name.to_owned(), json!(name)))
                .collect()
        };
        let identity = json!({
            "version":1,"seed_version":2,"seed_hash":"seed","lightning_address":"alice@example.invalid","archetype":"ronin",
            "user_traits":serde_json::to_value(&request).unwrap()["user_traits"],
            "derived_traits":string_fields("pose crop_style hat_style face_visibility eye_visibility weapon_mode shoulder_profile cloak_presence armor_wear enso_style ink_density brush_texture splash_intensity orange_placement ornament_level"),
            "resolved_directives":string_fields("composition silhouette face_treatment armor_treatment mood orange_treatment ink_treatment eyes_treatment background archetype_flavor")
        });
        let generate = server.mock(|when, then| {
            when.method(MockMethod::POST).path("/api/v1/connect/cubes/cube-7/avatar/generate")
                .json_body(serde_json::to_value(&request).unwrap());
            then.json_body(json!({"data":{"identity":identity,"variant":{"id":42,"index":0,"image_url":"/image/42"}}}));
        });
        let result = client
            .post_avatar_generate("cube-7", request)
            .await
            .unwrap();
        assert_eq!(result.identity.version, 1);
        assert_eq!(result.identity.seed_version, 2);
        assert_eq!(result.identity.lightning_address, "alice@example.invalid");
        assert_eq!(result.variant.id, 42);
        assert_eq!(result.variant.image_url, "/image/42");
        generate.assert();
    }

    #[tokio::test]
    async fn avatar_image_errors_are_not_returned_as_image_bytes() {
        let server = MockServer::start();
        let client = CoincubeClient::for_test(server.base_url());
        let image = server.mock(|when, then| {
            when.method(MockMethod::GET)
                .path("/api/v1/connect/avatar/image/9");
            then.status(404).body("missing image");
        });
        assert!(
            matches!(client.fetch_avatar_image(9).await, Err(CoincubeError::Unsuccessful(info)) if info.status_code == 404)
        );
        image.assert();
    }
}
