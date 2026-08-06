//! Optional TCP health evidence sensors.

use agent_api::ebpf::{TcpEvidenceMode, TcpHealthEvidenceSnapshot, TcpHealthSignal};
use agent_api::ontology::{
    EntityKind, EntityRef, EvidenceKind, OntologyEvidence, ONTOLOGY_SCHEMA_VERSION,
};

pub trait TcpHealthSensor: Send + Sync {
    fn snapshot(&self) -> TcpHealthEvidenceSnapshot;
}

pub struct UnavailableTcpHealthSensor;

impl TcpHealthSensor for UnavailableTcpHealthSensor {
    fn snapshot(&self) -> TcpHealthEvidenceSnapshot {
        TcpHealthEvidenceSnapshot {
            mode: TcpEvidenceMode::Unavailable,
            available: false,
            message: "TCP health sensors are disabled".to_string(),
            signals: vec![],
        }
    }
}

pub struct MockTcpHealthSensor;

impl TcpHealthSensor for MockTcpHealthSensor {
    fn snapshot(&self) -> TcpHealthEvidenceSnapshot {
        TcpHealthEvidenceSnapshot {
            mode: TcpEvidenceMode::Mock,
            available: true,
            message: "mock TCP health evidence".to_string(),
            signals: vec![tcp_signal("127.0.0.1:8080", Some(900), 0, 0, 0, 0)],
        }
    }
}

#[cfg(target_os = "linux")]
pub struct LinuxEbpfTcpHealthSensor;

#[cfg(target_os = "linux")]
impl TcpHealthSensor for LinuxEbpfTcpHealthSensor {
    fn snapshot(&self) -> TcpHealthEvidenceSnapshot {
        TcpHealthEvidenceSnapshot {
            mode: TcpEvidenceMode::LinuxEbpf,
            available: false,
            message: "Linux eBPF probe requires CAP_BPF/CAP_PERFMON or privileged execution; loader is not attached in this build".to_string(),
            signals: vec![],
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub struct LinuxEbpfTcpHealthSensor;

#[cfg(not(target_os = "linux"))]
impl TcpHealthSensor for LinuxEbpfTcpHealthSensor {
    fn snapshot(&self) -> TcpHealthEvidenceSnapshot {
        TcpHealthEvidenceSnapshot {
            mode: TcpEvidenceMode::LinuxEbpf,
            available: false,
            message: "Linux eBPF TCP health probe is only available on Linux".to_string(),
            signals: vec![],
        }
    }
}

pub fn sensor_from_env() -> Box<dyn TcpHealthSensor> {
    match std::env::var("RAUTA_TCP_EVIDENCE_MODE")
        .unwrap_or_else(|_| "unavailable".to_string())
        .as_str()
    {
        "mock" => Box::new(MockTcpHealthSensor),
        "linux-ebpf" => Box::new(LinuxEbpfTcpHealthSensor),
        _ => Box::new(UnavailableTcpHealthSensor),
    }
}

fn tcp_signal(
    backend: &str,
    rtt_us: Option<u64>,
    retransmits: u64,
    resets: u64,
    connection_failures: u64,
    congestion_events: u64,
) -> TcpHealthSignal {
    let mut evidence = OntologyEvidence::new(
        format!("tcp-health:{}", backend),
        EvidenceKind::BackendHealth,
        EntityRef::new(EntityKind::HealthSignal, format!("tcp:{}", backend)),
        "tcp-health-sensor",
    );
    evidence.schema_version = ONTOLOGY_SCHEMA_VERSION;
    if let Some(rtt) = rtt_us {
        evidence = evidence.with_attr("rtt_us", rtt);
    }
    evidence = evidence
        .with_attr("retransmits", retransmits)
        .with_attr("resets", resets)
        .with_attr("connection_failures", connection_failures)
        .with_attr("congestion_events", congestion_events);

    TcpHealthSignal {
        backend: backend.to_string(),
        rtt_us,
        retransmits,
        resets,
        connection_failures,
        congestion_events,
        ontology_evidence: vec![evidence],
    }
}
