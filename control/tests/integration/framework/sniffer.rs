//! Network packet capture using tcpdump for deep inspection

use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Network sniffer wrapper around tcpdump
pub struct Sniffer {
    process: Option<Child>,
    output_file: String,
    running: Arc<AtomicBool>,
}

impl Sniffer {
    /// Start tcpdump capture
    pub fn new(config: &super::super::SnifferConfig) -> Result<Self, Box<dyn std::error::Error>> {
        // Create output directory
        std::fs::create_dir_all(&config.output_dir)?;

        let output_file = format!(
            "{}/capture-{}.pcap",
            config.output_dir,
            chrono::Utc::now().format("%Y%m%d-%H%M%S")
        );

        println!("📡 Starting tcpdump: {} -> {}", config.filter, output_file);

        // Start tcpdump process
        let process = Command::new("tcpdump")
            .args(&["-i", &config.interface, "-w", &output_file, &config.filter])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;

        let running = Arc::new(AtomicBool::new(true));

        Ok(Self {
            process: Some(process),
            output_file,
            running,
        })
    }

    /// Stop capture and return output file path
    pub fn stop(&mut self) -> Result<String, Box<dyn std::error::Error>> {
        self.running.store(false, Ordering::SeqCst);

        if let Some(mut process) = self.process.take() {
            process.kill()?;
            process.wait()?;
        }

        println!("✅ tcpdump capture saved: {}", self.output_file);
        Ok(self.output_file.clone())
    }

    /// Get output file path
    pub fn output_file(&self) -> &str {
        &self.output_file
    }

    /// Analyze capture with tshark (Wireshark CLI)
    pub fn analyze(&self) -> Result<CaptureAnalysis, Box<dyn std::error::Error>> {
        let output = Command::new("tshark")
            .args(&[
                "-r",
                &self.output_file,
                "-q",
                "-z",
                "io,stat,1", // 1-second intervals
            ])
            .output()?;

        if !output.status.success() {
            return Err(
                format!("tshark failed: {}", String::from_utf8_lossy(&output.stderr)).into(),
            );
        }

        let stdout = String::from_utf8_lossy(&output.stdout);

        Ok(CaptureAnalysis {
            raw_output: stdout.to_string(),
            total_packets: Self::parse_total_packets(&stdout),
        })
    }

    fn parse_total_packets(output: &str) -> u64 {
        // Parse tshark output to extract total packets
        // This is a simplified parser
        output
            .lines()
            .filter(|line| line.contains("frames"))
            .filter_map(|line| {
                line.split_whitespace()
                    .nth(1)
                    .and_then(|s| s.parse::<u64>().ok())
            })
            .sum()
    }
}

impl Drop for Sniffer {
    fn drop(&mut self) {
        if let Err(e) = self.stop() {
            eprintln!("WARN: Failed to stop sniffer: {}", e);
        }
    }
}

#[derive(Debug)]
pub struct CaptureAnalysis {
    pub raw_output: String,
    pub total_packets: u64,
}
