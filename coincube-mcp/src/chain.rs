//! Exact supported chain identities. Address encodings alone do not identify a fork.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Chain {
    #[serde(rename = "bitcoin")]
    Bitcoin,
    #[serde(rename = "testnet4")]
    Testnet4,
    #[serde(rename = "regtest")]
    Regtest,
    #[serde(rename = "bitcoin-blake2b")]
    BitcoinBlake2b,
    #[serde(rename = "bitcoin-blake2b-testnet4")]
    BitcoinBlake2bTestnet4,
}

impl Chain {
    pub const ALL: [Self; 5] = [
        Self::Bitcoin,
        Self::Testnet4,
        Self::Regtest,
        Self::BitcoinBlake2b,
        Self::BitcoinBlake2bTestnet4,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bitcoin => "bitcoin",
            Self::Testnet4 => "testnet4",
            Self::Regtest => "regtest",
            Self::BitcoinBlake2b => "bitcoin-blake2b",
            Self::BitcoinBlake2bTestnet4 => "bitcoin-blake2b-testnet4",
        }
    }
}

impl std::str::FromStr for Chain {
    type Err = ();
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|chain| chain.as_str() == value)
            .ok_or(())
    }
}
