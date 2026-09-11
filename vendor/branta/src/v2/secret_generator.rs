//! Secret/nonce generation strategy, injectable for testing.

use uuid::Uuid;

#[cfg_attr(test, mockall::automock)]
pub trait SecretGeneratorTrait: Send + Sync {
    fn generate(&self) -> String;
    fn deterministic_nonce(&self) -> bool;
}

/// Production `SecretGeneratorTrait`: random UUID v4 secrets, non-deterministic nonces.
#[derive(Debug, Clone, Copy, Default)]
pub struct GuidSecretGenerator;

impl SecretGeneratorTrait for GuidSecretGenerator {
    fn generate(&self) -> String {
        Uuid::new_v4().to_string()
    }

    fn deterministic_nonce(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_a_valid_uuid_string() {
        let generator = GuidSecretGenerator;
        let value = generator.generate();
        assert!(Uuid::parse_str(&value).is_ok());
    }

    #[test]
    fn generates_unique_values() {
        let generator = GuidSecretGenerator;
        assert_ne!(generator.generate(), generator.generate());
    }

    #[test]
    fn deterministic_nonce_is_false() {
        assert!(!GuidSecretGenerator.deterministic_nonce());
    }
}
