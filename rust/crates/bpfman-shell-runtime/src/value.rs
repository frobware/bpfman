use crate::CommandResponse;
use std::{
    collections::BTreeMap,
    net::Ipv4Addr,
    num::{NonZeroU32, NonZeroU64},
};

use bpfman_shell_syntax::is_json_number;

/// A validated JSON number that retains its exact spelling and precision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Number(String);

impl Number {
    /// Accept one finite JSON number, including arbitrarily large integers.
    #[must_use]
    pub fn new(text: String) -> Option<Self> {
        is_json_number(&text).then_some(Self(text))
    }

    /// The exact decimal spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A background process capability and its immutable script-visible fields.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobHandle {
    id: NonZeroU64,
    fields: BTreeMap<String, Value>,
}

impl JobHandle {
    /// Associate a driver-issued job identity with the current process ID.
    #[must_use]
    pub fn new(id: NonZeroU64, pid: NonZeroU32, target_binary: Option<String>) -> Self {
        let mut fields =
            BTreeMap::from([("pid".to_owned(), Value::Number(Number(pid.to_string())))]);
        if let Some(binary) = target_binary {
            fields.insert("target_binary".to_owned(), Value::String(binary));
        }
        Self { id, fields }
    }

    /// Identify the job without using its potentially reusable process ID.
    #[must_use]
    pub fn id(&self) -> NonZeroU64 {
        self.id
    }
}

/// A leased kernel function fixture and its immutable script-visible fields.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KfuncHandle {
    id: NonZeroU64,
    fields: BTreeMap<String, Value>,
}

impl KfuncHandle {
    /// Associate a driver lease with the module's target fields.
    #[must_use]
    pub fn new(id: NonZeroU64, index: u8, name: String, trigger: String, count: String) -> Self {
        Self {
            id,
            fields: BTreeMap::from([
                ("index".to_owned(), Value::Number(Number(index.to_string()))),
                ("name".to_owned(), Value::String(name)),
                ("trigger".to_owned(), Value::String(trigger)),
                ("count".to_owned(), Value::String(count)),
            ]),
        }
    }

    /// Identify this lease independently of the reused slot index.
    #[must_use]
    pub fn id(&self) -> NonZeroU64 {
        self.id
    }
}

/// A capability for a private LSM fixture and its stable projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LsmHandle {
    id: NonZeroU64,
    fields: BTreeMap<String, Value>,
}

impl LsmHandle {
    /// Associate an owned marker and target file with the driver lease.
    #[must_use]
    pub fn new(id: NonZeroU64, marker: String, path: String, directory: String) -> Self {
        let marker_hex = marker.bytes().map(|byte| format!("{byte:02x}")).collect();
        Self {
            id,
            fields: BTreeMap::from([
                ("marker".to_owned(), Value::String(marker)),
                ("marker_hex".to_owned(), Value::String(marker_hex)),
                ("file".to_owned(), Value::String(path)),
                ("dir".to_owned(), Value::String(directory)),
            ]),
        }
    }

    /// Identify the fixture without accepting a path or marker as authority.
    #[must_use]
    pub fn id(&self) -> NonZeroU64 {
        self.id
    }
}

/// One endpoint of an isolated veth pair.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EndpointSide {
    /// The first endpoint.
    A,
    /// The second endpoint.
    B,
}

/// Which operation a network handle can authorise.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkRole {
    /// A pair with one host end and one namespaced peer.
    HostPair,
    /// A pair can be released.
    Pair,
    /// An endpoint can run a command in its namespace.
    Endpoint(EndpointSide),
}

/// The observed fields of one veth end in a named namespace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkEndpoint {
    /// The owned namespace name.
    pub namespace: String,
    /// The interface name.
    pub interface: String,
    /// Address on the private fixture subnet.
    pub address: Ipv4Addr,
    /// Interface index in its namespace, or zero when unavailable.
    pub interface_index: u32,
    /// Namespace inode, or zero when unavailable.
    pub namespace_id: u64,
}

/// The observed fields of the network fixture with one endpoint on the host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostNetworkPair {
    /// The peer's namespace.
    pub namespace: String,
    /// The host-side interface name.
    pub host_link: String,
    /// The peer-side interface name.
    pub peer_link: String,
    /// The host-side address.
    pub host_address: Ipv4Addr,
    /// The peer-side address.
    pub peer_address: Ipv4Addr,
    /// The host's interface index, or zero when unavailable.
    pub host_interface_index: u32,
    /// The host network namespace inode, or zero when unavailable.
    pub host_namespace_id: u64,
}

/// An opaque capability and record projection for a network fixture.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkHandle {
    id: NonZeroU64,
    role: NetworkRole,
    fields: BTreeMap<String, Value>,
}

impl NetworkHandle {
    /// Build the capability for a host and peer topology.
    #[must_use]
    pub fn host_pair(id: NonZeroU64, pair: HostNetworkPair) -> Self {
        Self {
            id,
            role: NetworkRole::HostPair,
            fields: BTreeMap::from([
                ("ns".to_owned(), Value::String(pair.namespace)),
                ("host_link".to_owned(), Value::String(pair.host_link)),
                ("peer_link".to_owned(), Value::String(pair.peer_link)),
                (
                    "host_addr".to_owned(),
                    Value::String(pair.host_address.to_string()),
                ),
                (
                    "peer_addr".to_owned(),
                    Value::String(pair.peer_address.to_string()),
                ),
                (
                    "host_ifindex".to_owned(),
                    Value::Number(Number(pair.host_interface_index.to_string())),
                ),
                (
                    "host_nsid".to_owned(),
                    Value::Number(Number(pair.host_namespace_id.to_string())),
                ),
            ]),
        }
    }

    /// Build a pair whose endpoints are tied to the same driver-issued lease.
    #[must_use]
    pub fn pair(id: NonZeroU64, a: NetworkEndpoint, b: NetworkEndpoint) -> Self {
        Self {
            id,
            role: NetworkRole::Pair,
            fields: BTreeMap::from([
                (
                    "a".to_owned(),
                    Value::Network(Self::endpoint(id, EndpointSide::A, a)),
                ),
                (
                    "b".to_owned(),
                    Value::Network(Self::endpoint(id, EndpointSide::B, b)),
                ),
            ]),
        }
    }

    fn endpoint(id: NonZeroU64, side: EndpointSide, endpoint: NetworkEndpoint) -> Self {
        Self {
            id,
            role: NetworkRole::Endpoint(side),
            fields: BTreeMap::from([
                ("ns".to_owned(), Value::String(endpoint.namespace)),
                ("link".to_owned(), Value::String(endpoint.interface)),
                (
                    "addr".to_owned(),
                    Value::String(endpoint.address.to_string()),
                ),
                (
                    "ifindex".to_owned(),
                    Value::Number(Number(endpoint.interface_index.to_string())),
                ),
                (
                    "nsid".to_owned(),
                    Value::Number(Number(endpoint.namespace_id.to_string())),
                ),
            ]),
        }
    }

    /// Identify the lease shared by the pair and both endpoints.
    #[must_use]
    pub fn id(&self) -> NonZeroU64 {
        self.id
    }

    /// Determine whether this capability refers to a pair or endpoint.
    #[must_use]
    pub fn role(&self) -> NetworkRole {
        self.role
    }
}

/// An evaluated shell value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Value {
    /// No value was produced.
    Absent,
    /// Explicit JSON null.
    Null,
    /// Boolean value.
    Bool(bool),
    /// Exact number.
    Number(Number),
    /// String value.
    String(String),
    /// Ordered list.
    List(Vec<Value>),
    /// Named fields.
    Record(BTreeMap<String, Value>),
    /// A bpfman CLI record with a resolved ID for subsequent commands.
    BpfmanRecord {
        /// Fields from the CLI's JSON projection.
        fields: BTreeMap<String, Value>,
        /// Program or link ID accepted by the CLI.
        id: Number,
    },
    /// A capability for a background process owned by the driver.
    Job(JobHandle),
    /// A capability for a leased kernel function fixture.
    Kfunc(KfuncHandle),
    /// A capability for a private LSM fixture.
    Lsm(LsmHandle),
    /// A capability for a network fixture or an endpoint of it.
    Network(NetworkHandle),
    /// Captured command outcome, with a distinct primary when supplied.
    Outcome(Box<CommandResponse>),
}

impl Value {
    /// Named fields of a record or a typed value with a record projection.
    #[must_use]
    pub fn record_fields(&self) -> Option<&BTreeMap<String, Self>> {
        match self {
            Self::Record(fields) | Self::BpfmanRecord { fields, .. } => Some(fields),
            Self::Job(job) => Some(&job.fields),
            Self::Kfunc(function) => Some(&function.fields),
            Self::Lsm(probe) => Some(&probe.fields),
            Self::Network(handle) => Some(&handle.fields),
            _ => None,
        }
    }

    /// Render a scalar while preserving exact number spellings.
    #[must_use]
    pub fn scalar_text(&self) -> Option<String> {
        match self {
            Self::Null => Some("null".to_owned()),
            Self::Bool(value) => Some(value.to_string()),
            Self::Number(value) => Some(value.as_str().to_owned()),
            Self::String(value) => Some(value.clone()),
            _ => None,
        }
    }
}

/// One argv operand with its evaluation origin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Argument {
    /// An unquoted argv word.
    Word(String),
    /// A quoted argv word.
    Quoted(String),
    /// An evaluated value.
    Value(Value),
    /// Driver adapter with its already resolved value.
    Adapter {
        /// Adapter name.
        name: String,
        /// Value that the adapter will render or consume.
        value: Value,
    },
}
