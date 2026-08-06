//! Optional eBPF-derived TCP health evidence.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ontology::OntologyEvidence;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TcpHealthEvidenceSnapshot {
    pub mode: TcpEvidenceMode,
    pub available: bool,
    pub message: String,
    pub signals: Vec<TcpHealthSignal>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TcpEvidenceMode {
    Unavailable,
    Mock,
    LinuxEbpf,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TcpHealthSignal {
    pub backend: String,
    pub rtt_us: Option<u64>,
    pub retransmits: u64,
    pub resets: u64,
    pub connection_failures: u64,
    pub congestion_events: u64,
    pub ontology_evidence: Vec<OntologyEvidence>,
}
