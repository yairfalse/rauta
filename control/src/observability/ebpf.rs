//! Optional TCP health evidence sensors.

use agent_api::ebpf::{TcpEvidenceMode, TcpHealthEvidenceSnapshot, TcpHealthSignal};
use agent_api::ontology::{
    EntityKind, EntityRef, EvidenceKind, OntologyEvidence, ONTOLOGY_SCHEMA_VERSION,
};

const DEFAULT_MOCK_BACKEND: &str = "127.0.0.1:8080";
const CAP_SYS_ADMIN: u8 = 21;
const CAP_PERFMON: u8 = 38;
const CAP_BPF: u8 = 39;

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
        let signal = mock_signal_from_env();
        TcpHealthEvidenceSnapshot {
            mode: TcpEvidenceMode::Mock,
            available: true,
            message: "mock TCP health evidence from deterministic environment inputs".to_string(),
            signals: vec![signal],
        }
    }
}

#[cfg(target_os = "linux")]
pub struct LinuxEbpfTcpHealthSensor;

#[cfg(target_os = "linux")]
impl TcpHealthSensor for LinuxEbpfTcpHealthSensor {
    fn snapshot(&self) -> TcpHealthEvidenceSnapshot {
        let capability_state = linux_capability_state();
        let message = if capability_state.has_probe_capability {
            "Linux eBPF probe capability detected, but loader is not attached in this build"
                .to_string()
        } else {
            format!(
                "Linux eBPF probe requires CAP_BPF/CAP_PERFMON or CAP_SYS_ADMIN; missing {}",
                capability_state.missing.join("/")
            )
        };

        TcpHealthEvidenceSnapshot {
            mode: TcpEvidenceMode::LinuxEbpf,
            available: false,
            message,
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
    match normalized_mode(std::env::var("RAUTA_TCP_EVIDENCE_MODE").as_deref().ok()) {
        "mock" => Box::new(MockTcpHealthSensor),
        "linux-ebpf" => Box::new(LinuxEbpfTcpHealthSensor),
        _ => Box::new(UnavailableTcpHealthSensor),
    }
}

fn normalized_mode(value: Option<&str>) -> &'static str {
    match value
        .unwrap_or("unavailable")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "mock" => "mock",
        "linux-ebpf" | "linux_ebpf" | "ebpf" => "linux-ebpf",
        _ => "unavailable",
    }
}

fn mock_signal_from_env() -> TcpHealthSignal {
    tcp_signal(
        &std::env::var("RAUTA_TCP_EVIDENCE_MOCK_BACKEND")
            .unwrap_or_else(|_| DEFAULT_MOCK_BACKEND.to_string()),
        env_u64("RAUTA_TCP_EVIDENCE_MOCK_RTT_US").or(Some(900)),
        env_u64("RAUTA_TCP_EVIDENCE_MOCK_RETRANSMITS").unwrap_or_default(),
        env_u64("RAUTA_TCP_EVIDENCE_MOCK_RESETS").unwrap_or_default(),
        env_u64("RAUTA_TCP_EVIDENCE_MOCK_CONNECTION_FAILURES").unwrap_or_default(),
        env_u64("RAUTA_TCP_EVIDENCE_MOCK_CONGESTION_EVENTS").unwrap_or_default(),
    )
}

fn env_u64(key: &str) -> Option<u64> {
    std::env::var(key).ok()?.parse().ok()
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct LinuxCapabilityState {
    has_probe_capability: bool,
    missing: Vec<&'static str>,
}

#[cfg(target_os = "linux")]
fn linux_capability_state() -> LinuxCapabilityState {
    linux_capability_state_from_status(
        &std::fs::read_to_string("/proc/self/status").unwrap_or_default(),
    )
}

#[cfg(target_os = "linux")]
fn linux_capability_state_from_status(status: &str) -> LinuxCapabilityState {
    let effective = status
        .lines()
        .find_map(|line| line.strip_prefix("CapEff:"))
        .and_then(|value| u64::from_str_radix(value.trim(), 16).ok())
        .unwrap_or_default();
    let has_sys_admin = has_capability(effective, CAP_SYS_ADMIN);
    let has_bpf_pair = has_capability(effective, CAP_BPF) && has_capability(effective, CAP_PERFMON);
    let has_probe_capability = has_sys_admin || has_bpf_pair;
    let mut missing = Vec::new();
    if !has_probe_capability {
        if !has_capability(effective, CAP_BPF) {
            missing.push("CAP_BPF");
        }
        if !has_capability(effective, CAP_PERFMON) {
            missing.push("CAP_PERFMON");
        }
        if !has_sys_admin {
            missing.push("CAP_SYS_ADMIN");
        }
    }

    LinuxCapabilityState {
        has_probe_capability,
        missing,
    }
}

#[cfg(target_os = "linux")]
fn has_capability(effective: u64, capability: u8) -> bool {
    effective & (1u64 << capability) != 0
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_mode_accepts_expected_aliases() {
        assert_eq!(normalized_mode(None), "unavailable");
        assert_eq!(normalized_mode(Some("mock")), "mock");
        assert_eq!(normalized_mode(Some(" MOCK ")), "mock");
        assert_eq!(normalized_mode(Some("linux-ebpf")), "linux-ebpf");
        assert_eq!(normalized_mode(Some("linux_ebpf")), "linux-ebpf");
        assert_eq!(normalized_mode(Some("ebpf")), "linux-ebpf");
        assert_eq!(normalized_mode(Some("unexpected")), "unavailable");
    }

    #[test]
    fn tcp_signal_attaches_ontology_attributes() {
        let signal = tcp_signal("10.0.0.1:8080", Some(251_000), 2, 1, 0, 3);
        assert_eq!(signal.backend, "10.0.0.1:8080");
        assert_eq!(signal.rtt_us, Some(251_000));
        assert_eq!(signal.retransmits, 2);
        assert_eq!(signal.resets, 1);
        assert_eq!(signal.congestion_events, 3);

        assert_eq!(signal.ontology_evidence.len(), 1);
        let evidence = &signal.ontology_evidence[0];
        assert_eq!(evidence.schema_version, ONTOLOGY_SCHEMA_VERSION);
        assert_eq!(evidence.attributes.get("rtt_us"), Some(&251_000u64.into()));
        assert_eq!(evidence.attributes.get("retransmits"), Some(&2u64.into()));
        assert_eq!(evidence.attributes.get("resets"), Some(&1u64.into()));
        assert_eq!(
            evidence.attributes.get("congestion_events"),
            Some(&3u64.into())
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_capability_state_accepts_sys_admin() {
        let cap_eff = 1u64 << CAP_SYS_ADMIN;
        let state = linux_capability_state_from_status(&format!("CapEff:\t{cap_eff:016x}\n"));
        assert!(state.has_probe_capability);
        assert!(state.missing.is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_capability_state_accepts_bpf_and_perfmon_pair() {
        let cap_eff = (1u64 << CAP_BPF) | (1u64 << CAP_PERFMON);
        let state = linux_capability_state_from_status(&format!("CapEff:\t{cap_eff:016x}\n"));
        assert!(state.has_probe_capability);
        assert!(state.missing.is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_capability_state_reports_missing_probe_caps() {
        let state = linux_capability_state_from_status("CapEff:\t0000000000000000\n");
        assert!(!state.has_probe_capability);
        assert_eq!(
            state.missing,
            vec!["CAP_BPF", "CAP_PERFMON", "CAP_SYS_ADMIN"]
        );
    }
}
