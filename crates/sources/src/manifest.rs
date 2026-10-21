//! What a plugin declares about itself (architecture §18.1). The settings
//! UI shows the endpoints, risk and consent class before a plugin can be
//! enabled.

/// What a plugin does. One plugin can be several kinds: the Mainline DHT
/// both discovers keys and finds peers for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Emits keys (plus optional claims) as it finds them.
    Discovery,
    /// Like `Discovery`, run as a resumable job with progress.
    Importer,
    /// Finds peers for keys that have none yet.
    PeerFinder,
    Enrichment,
    /// A whole network tier with its own keys, fetch and link format.
    Network,
    Output,
}

/// What enabling the plugin requires (§18.7), from least to most.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Consent {
    None,
    External,
    Account,
    Restricted,
    Overlay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Risk {
    Low,
    Medium,
    High,
}

/// §18.6 network tiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Network {
    Bt,
    BtI2p,
    Ed2k,
    Ipfs,
    Usenet,
}

/// An external endpoint the plugin contacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Endpoint {
    pub protocol: &'static str,
    pub host: &'static str,
}

/// A source's share of the fetch pool (§18.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Priority {
    Low,
    #[default]
    Normal,
    High,
    Max,
}

impl Priority {
    /// The deficit-round-robin weight.
    pub fn weight(self) -> u32 {
        match self {
            Priority::Low => 1,
            Priority::Normal => 3,
            Priority::High => 6,
            Priority::Max => 10,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// Stable id, recorded in the store's `sources` table. Never rename.
    pub id: &'static str,
    pub name: &'static str,
    pub version: &'static str,
    pub kinds: &'static [Kind],
    pub networks: &'static [Network],
    pub endpoints: &'static [Endpoint],
    pub risk: Risk,
    pub consent: Consent,
    pub default_priority: Priority,
    /// Only the Mainline DHT is on out of the box (§18.1).
    pub enabled_by_default: bool,
    /// Names of the plugin's harvest strategy codes ([`Found::strategy`]),
    /// indexed by code, for metric labels.
    ///
    /// [`Found::strategy`]: crate::Found::strategy
    pub strategies: &'static [&'static str],
}

impl Manifest {
    pub fn is(&self, kind: Kind) -> bool {
        self.kinds.contains(&kind)
    }

    /// A strategy code's name, or `None` for a code the manifest doesn't list.
    pub fn strategy_name(&self, code: u8) -> Option<&'static str> {
        self.strategies.get(usize::from(code)).copied()
    }
}
