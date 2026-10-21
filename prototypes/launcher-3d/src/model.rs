use std::fmt;

pub const MAX_PER_NETWORK: usize = 7;
pub const MAX_CUBES: usize = MAX_PER_NETWORK * Network::ALL.len();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Network {
    Bitcoin,
    Blake2b,
    Testnet,
}

impl Network {
    pub const ALL: [Self; 3] = [Self::Bitcoin, Self::Blake2b, Self::Testnet];

    pub fn color(self) -> [f32; 4] {
        match self {
            Self::Bitcoin => [0.87, 0.42, 0.12, 1.0],
            Self::Blake2b => [0.25, 0.56, 0.64, 1.0],
            Self::Testnet => [0.43, 0.46, 0.58, 1.0],
        }
    }
}

impl fmt::Display for Network {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Bitcoin => "Bitcoin mainnet",
            Self::Blake2b => "Bitcoin Blake2b mainnet",
            Self::Testnet => "Testnet4 · test coins",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    All,
    Live,
    Bitcoin,
    Blake2b,
    Tests,
}

impl Filter {
    pub const ALL: [Self; 5] = [
        Self::All,
        Self::Live,
        Self::Bitcoin,
        Self::Blake2b,
        Self::Tests,
    ];

    pub fn accepts(self, network: Network) -> bool {
        match self {
            Self::All => true,
            Self::Live => network != Network::Testnet,
            Self::Bitcoin => network == Network::Bitcoin,
            Self::Blake2b => network == Network::Blake2b,
            Self::Tests => network == Network::Testnet,
        }
    }
}

impl fmt::Display for Filter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::All => "All networks",
            Self::Live => "Live chains",
            Self::Bitcoin => "Bitcoin only",
            Self::Blake2b => "Blake2b only",
            Self::Tests => "Test networks",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protection {
    Confirmed,
    NeedsBackup,
    Unknown,
}

impl fmt::Display for Protection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Confirmed => "Backup confirmed · fixture",
            Self::NeedsBackup => "Backup not confirmed",
            Self::Unknown => "Backup status unknown",
        })
    }
}

#[derive(Debug, Clone)]
pub struct Cube {
    pub id: u32,
    pub name: String,
    pub network: Network,
    pub protection: Protection,
    pub has_vault: bool,
}

pub fn fixture(id: u32) -> Cube {
    let names = [
        "Everyday",
        "Blake workspace",
        "Lab",
        "Family savings",
        "Travel",
        "Archive",
    ];
    let network = Network::ALL[id as usize % 3];
    Cube {
        id,
        name: if (id as usize) < names.len() {
            names[id as usize].into()
        } else {
            format!("Cube {:03}", id + 1)
        },
        network,
        protection: match id % 4 {
            0 | 3 => Protection::Confirmed,
            1 => Protection::NeedsBackup,
            _ => Protection::Unknown,
        },
        has_vault: id % 3 != 2,
    }
}

#[derive(Debug, Default)]
pub struct Catalog {
    pub cubes: Vec<Cube>,
    next_id: u32,
}

impl Catalog {
    pub fn network_count(&self, network: Network) -> usize {
        self.cubes
            .iter()
            .filter(|cube| cube.network == network)
            .count()
    }

    pub fn can_create(&self, network: Network) -> bool {
        self.network_count(network) < MAX_PER_NETWORK
    }

    pub fn reset(&mut self, count: u32) {
        let count = count.min(MAX_CUBES as u32);
        self.cubes = (0..count).map(fixture).collect();
        self.next_id = count;
    }

    pub fn create(&mut self, network: Network) -> Option<u32> {
        if !self.can_create(network) {
            return None;
        }
        let id = self.next_id;
        self.next_id += 1;
        let mut cube = fixture(id);
        cube.network = network;
        cube.name = format!("New Cube {}", id + 1);
        cube.protection = Protection::NeedsBackup;
        cube.has_vault = false;
        self.cubes.push(cube);
        Some(id)
    }

    pub fn visible(&self, filter: Filter) -> Vec<Cube> {
        self.cubes
            .iter()
            .filter(|c| filter.accepts(c.network))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_includes_both_mainnets_and_excludes_test_coins() {
        let mut catalog = Catalog::default();
        catalog.reset(20);
        let live = catalog.visible(Filter::Live);
        assert!(live.iter().any(|c| c.network == Network::Bitcoin));
        assert!(live.iter().any(|c| c.network == Network::Blake2b));
        assert!(live.iter().all(|c| c.network != Network::Testnet));
        assert_eq!(live.len() + catalog.visible(Filter::Tests).len(), 20);
    }

    #[test]
    fn creation_uses_explicit_network_and_unique_identity_after_reset() {
        let mut catalog = Catalog::default();
        catalog.reset(4);
        catalog.create(Network::Testnet);
        let id = catalog.create(Network::Blake2b).unwrap();
        let created = catalog.cubes.iter().find(|c| c.id == id).unwrap();
        assert_eq!(created.network, Network::Blake2b);
        assert_eq!(created.protection, Protection::NeedsBackup);
        assert!(!created.has_vault);
        let ids: std::collections::HashSet<_> = catalog.cubes.iter().map(|c| c.id).collect();
        assert_eq!(ids.len(), catalog.cubes.len());
        catalog.reset(1);
        assert_eq!(catalog.create(Network::Bitcoin), Some(1));
    }

    #[test]
    fn catalog_never_exceeds_gpu_instance_capacity() {
        let mut catalog = Catalog::default();
        catalog.reset(u32::MAX);
        assert_eq!(catalog.cubes.len(), MAX_CUBES);
        assert_eq!(catalog.create(Network::Bitcoin), None);
        assert_eq!(catalog.cubes.len(), MAX_CUBES);
    }

    #[test]
    fn estate_fixture_limit_applies_independently_to_each_network() {
        let mut catalog = Catalog::default();
        for network in Network::ALL {
            for _ in 0..MAX_PER_NETWORK {
                assert!(catalog.create(network).is_some());
            }
            assert_eq!(catalog.network_count(network), MAX_PER_NETWORK);
            assert!(!catalog.can_create(network));
            assert_eq!(catalog.create(network), None);
        }
        assert_eq!(catalog.cubes.len(), MAX_CUBES);
        catalog.reset(20);
        assert!(!catalog.can_create(Network::Bitcoin));
        assert!(!catalog.can_create(Network::Blake2b));
        assert!(catalog.create(Network::Testnet).is_some());
        assert!(!catalog.can_create(Network::Testnet));
    }
}
