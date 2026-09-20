//! Registry that owns the app's wallet backends and exposes routing hooks.
//!
//! Holds an optional [`LiquidBackend`] and an optional
//! [`SparkBackend`] (present when the cube has a Spark signer and
//! the bridge subprocess spawned successfully).
//! [`WalletRegistry::route_lightning_address`] returns the backend that
//! should fulfill the next incoming Lightning Address invoice: Spark
//! when available, Liquid when configured, or no route for Vault-only Cubes.
//!
//! The registry is the single place the app decides *which* backend
//! handles *which* payment type — keeping that logic in one module
//! means the routing policy is a one-file change.

use std::sync::Arc;

use super::liquid::LiquidBackend;
use super::spark::SparkBackend;

/// Which backend a routing decision picked. Carries the backend handle
/// so callers don't have to re-resolve it from the registry.
#[derive(Clone)]
pub enum LightningRoute {
    Liquid(Arc<LiquidBackend>),
    Spark(Arc<SparkBackend>),
}

/// Owns the per-cube wallet backends.
///
/// Cheap to clone — the backends live behind `Arc`s so clones share state.
#[derive(Clone)]
pub struct WalletRegistry {
    liquid: Option<Arc<LiquidBackend>>,
    /// `None` if the cube has no Spark signer configured, or if the
    /// bridge subprocess failed to spawn / handshake. Panels code
    /// checks this and shows a "Spark unavailable" placeholder when
    /// absent.
    spark: Option<Arc<SparkBackend>>,
}

impl WalletRegistry {
    /// A Vault-only Cube has no Liquid or Spark backend and no Lightning route.
    pub fn vault_only() -> Self {
        Self {
            liquid: None,
            spark: None,
        }
    }

    pub fn new(liquid: Arc<LiquidBackend>) -> Self {
        Self {
            liquid: Some(liquid),
            spark: None,
        }
    }

    pub fn with_spark(liquid: Arc<LiquidBackend>, spark: Option<Arc<SparkBackend>>) -> Self {
        Self {
            liquid: Some(liquid),
            spark,
        }
    }

    /// Access the Liquid backend.
    pub fn liquid(&self) -> Option<&Arc<LiquidBackend>> {
        self.liquid.as_ref()
    }

    /// Access the Spark backend, if the bridge is up for this cube.
    pub fn spark(&self) -> Option<&Arc<SparkBackend>> {
        self.spark.as_ref()
    }

    /// Returns the backend that should fulfill incoming Lightning Address
    /// requests: Spark when the bridge is up, Liquid otherwise.
    ///
    /// Falling back to Liquid keeps invoice requests answerable even when
    /// the Spark setup is broken (no signer, subprocess crashed, etc.).
    pub fn route_lightning_address(&self) -> Option<LightningRoute> {
        match self.spark.clone() {
            Some(spark) => Some(LightningRoute::Spark(spark)),
            None => self.liquid.clone().map(LightningRoute::Liquid),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vault_only_has_no_secondary_backend_or_lightning_fallback() {
        let registry = WalletRegistry::vault_only();
        assert!(registry.liquid().is_none());
        assert!(registry.spark().is_none());
        assert!(registry.route_lightning_address().is_none());
    }
}
