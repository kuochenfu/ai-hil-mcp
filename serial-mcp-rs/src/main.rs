use anyhow::Result;
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::Deserialize;
use std::{
    io::{BufRead, BufReader, Write},
    time::{Duration, Instant},
};

const ANOMALY_KEYWORDS: &[&str] = &[
    "panic", "hardfault", "assert", "watchdog", "stack overflow", "hard fault",
];

fn flag_anomalies(lines: &[String]) -> String {
    let anomalies: Vec<&String> = lines
        .iter()
        .filter(|l| {
            let lower = l.to_lowercase();
            ANOMALY_KEYWORDS.iter().any(|k| lower.contains(k))
        })
        .collect();

    let body = lines.join("\n");
    if anomalies.is_empty() {
        body
    } else {
        let warning = format!(
            "WARNING: ANOMALY DETECTED:\n{}\n---\n",
            anomalies.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n")
        );
        warning + &body
    }
}

// ── tool implementations ─────────────────────────────────────────────────────

fn do_list_serial_ports() -> Result<String> {
    let ports = serialport::available_ports()?;
    if ports.is_empty() {
        return Ok("No serial ports found.".to_string());
    }
    let lines: Vec<String> = ports
        .iter()
        .map(|p| {
            let desc = match &p.port_type {
                serialport::SerialPortType::UsbPort(info) => format!(
                    "USB — {} {}",
                    info.manufacturer.as_deref().unwrap_or("?"),
                    info.product.as_deref().unwrap_or("?")
                ),
                serialport::SerialPortType::BluetoothPort => "Bluetooth".to_string(),
                serialport::SerialPortType::PciPort => "PCI".to_string(),
                serialport::SerialPortType::Unknown => "Unknown".to_string(),
            };
            format!("{} — {}", p.port_name, desc)
        })
        .collect();
    Ok(lines.join("\n"))
}

fn do_read_serial_log(port: &str, baud: u32, lines: usize, timeout_s: u64) -> Result<String> {
    let serial = serialport::new(port, baud)
        .timeout(Duration::from_secs(1))
        .open()
        .map_err(|e| anyhow::anyhow!("Could not open {}: {}", port, e))?;

    let mut reader = BufReader::new(serial);
    let mut buffer: Vec<String> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(timeout_s);

    while Instant::now() < deadline && buffer.len() < lines {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim().to_string();
                if !trimmed.is_empty() {
                    buffer.push(trimmed);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
            Err(_) => break,
        }
    }

    if buffer.is_empty() {
        return Ok("No data received. Check that the device is running and baud rate is correct.".to_string());
    }
    Ok(flag_anomalies(&buffer))
}

fn do_send_serial_command(port: &str, command: &str, baud: u32, response_lines: usize) -> Result<String> {
    let mut serial = serialport::new(port, baud)
        .timeout(Duration::from_secs(2))
        .open()
        .map_err(|e| anyhow::anyhow!("Could not open {}: {}", port, e))?;

    let cmd = format!("{}\r\n", command);
    serial.write_all(cmd.as_bytes())?;
    serial.flush()?;

    let mut reader = BufReader::new(serial);
    let mut buffer: Vec<String> = Vec::new();

    for _ in 0..response_lines {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim().to_string();
                if !trimmed.is_empty() {
                    buffer.push(trimmed);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => break,
            Err(_) => break,
        }
    }

    if buffer.is_empty() {
        return Ok("Command sent. No response received.".to_string());
    }
    Ok(flag_anomalies(&buffer))
}

// ── MCP server ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct SerialMcp {
    tool_router: ToolRouter<Self>,
}

impl SerialMcp {
    fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ReadSerialLogParams {
    /// Serial port path (e.g. /dev/tty.usbmodem1303)
    port: String,
    /// Baud rate (default: 115200)
    #[serde(default = "default_baud")]
    baud: u32,
    /// Maximum number of lines to collect (default: 50)
    #[serde(default = "default_lines")]
    lines: usize,
    /// Total read window in seconds (default: 8)
    #[serde(default = "default_timeout")]
    timeout_s: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SendCommandParams {
    /// Serial port path (e.g. /dev/tty.usbmodem1303)
    port: String,
    /// Command string to send
    command: String,
    /// Baud rate (default: 115200)
    #[serde(default = "default_baud")]
    baud: u32,
    /// Number of response lines to collect (default: 10)
    #[serde(default = "default_response_lines")]
    response_lines: usize,
}

fn default_baud() -> u32 { 115200 }
fn default_lines() -> usize { 50 }
fn default_timeout() -> u64 { 8 }
fn default_response_lines() -> usize { 10 }

#[tool_router]
impl SerialMcp {
    #[tool(description = "List all available serial ports on this machine.")]
    async fn list_serial_ports(&self) -> String {
        do_list_serial_ports().unwrap_or_else(|e| format!("ERROR: {}", e))
    }

    #[tool(description = "Read up to N lines from a serial port within a time window. Auto-flags anomalies like HardFault, Panic, Watchdog Reset.")]
    async fn read_serial_log(&self, Parameters(p): Parameters<ReadSerialLogParams>) -> String {
        do_read_serial_log(&p.port, p.baud, p.lines, p.timeout_s)
            .unwrap_or_else(|e| format!("ERROR: {}", e))
    }

    #[tool(description = "Send a command over serial and return the response.")]
    async fn send_serial_command(&self, Parameters(p): Parameters<SendCommandParams>) -> String {
        do_send_serial_command(&p.port, &p.command, p.baud, p.response_lines)
            .unwrap_or_else(|e| format!("ERROR: {}", e))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for SerialMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let server = SerialMcp::new();
    let transport = stdio();
    let handle = server.serve(transport).await?;
    handle.waiting().await?;
    Ok(())
}
