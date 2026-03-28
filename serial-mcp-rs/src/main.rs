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
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

// ── timestamp formatting ──────────────────────────────────────────────────────

fn format_time_ms(t: SystemTime) -> String {
    let dur = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = dur.as_secs();
    let ms = dur.subsec_millis();
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{:02}:{:02}:{:02}.{:03}", h, m, s, ms)
}

// ── anomaly detection ─────────────────────────────────────────────────────────

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

// ── device registry ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Default)]
struct BoardConfig {
    log_port: String,
    shell_port: Option<String>,
    /// Baud rate override for this board (default: 115200)
    baud: Option<u32>,
    description: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct RawRegistry {
    #[serde(default)]
    board: HashMap<String, BoardConfig>,
}

#[derive(Debug, Clone, Default)]
struct DeviceRegistry {
    boards: HashMap<String, BoardConfig>,
}

fn load_registry() -> DeviceRegistry {
    let path = std::env::var("SERIAL_MCP_CONFIG")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::current_exe().ok().and_then(|exe| {
                let candidate = exe.parent()?.join("devices.toml");
                candidate.exists().then_some(candidate)
            })
        });

    let path = match path {
        Some(p) => p,
        None => return DeviceRegistry::default(),
    };

    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("serial-mcp: could not read {:?}: {}", path, e);
            return DeviceRegistry::default();
        }
    };

    let raw: RawRegistry = match toml::from_str(&content) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("serial-mcp: could not parse {:?}: {}", path, e);
            return DeviceRegistry::default();
        }
    };

    DeviceRegistry { boards: raw.board }
}

/// Resolve an alias ("ping/log", "pong/shell") or a raw path ("/dev/cu.xxx").
/// Returns (port_path, baud, display_label).
fn resolve_port(alias: &str, registry: &DeviceRegistry) -> Result<(String, u32, String)> {
    if let Some((board_name, channel)) = alias.split_once('/') {
        let board = registry.boards.get(board_name).ok_or_else(|| {
            let known: Vec<_> = registry.boards.keys().collect();
            anyhow::anyhow!(
                "Unknown board '{}'. Configured boards: [{}]. Run list_boards for details.",
                board_name,
                known.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            )
        })?;
        let baud = board.baud.unwrap_or(115200);
        let port = match channel {
            "log" => board.log_port.clone(),
            "shell" => board
                .shell_port
                .clone()
                .ok_or_else(|| anyhow::anyhow!("Board '{}' has no shell_port configured.", board_name))?,
            _ => {
                return Err(anyhow::anyhow!(
                    "Unknown channel '{}'. Use 'log' or 'shell'.",
                    channel
                ))
            }
        };
        Ok((port, baud, alias.to_string()))
    } else {
        Ok((alias.to_string(), 115200, alias.to_string()))
    }
}

// ── serial implementations ────────────────────────────────────────────────────

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
                    "USB — {} {} [serial: {}]",
                    info.manufacturer.as_deref().unwrap_or("?"),
                    info.product.as_deref().unwrap_or("?"),
                    info.serial_number.as_deref().unwrap_or("?"),
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

fn do_list_boards(registry: &DeviceRegistry) -> String {
    if registry.boards.is_empty() {
        return "No boards configured. Set SERIAL_MCP_CONFIG to a devices.toml path.".to_string();
    }

    let mut lines = vec!["Configured boards:".to_string()];
    let mut names: Vec<_> = registry.boards.keys().collect();
    names.sort();

    for name in names {
        let b = &registry.boards[name];
        let desc = b.description.as_deref().unwrap_or("");
        let baud = b.baud.unwrap_or(115200);
        lines.push(format!("  {}  {}", name, desc));
        lines.push(format!("    log_port:   {} @ {} baud  →  use alias '{}/log'", b.log_port, baud, name));
        if let Some(sp) = &b.shell_port {
            lines.push(format!("    shell_port: {} @ {} baud  →  use alias '{}/shell'", sp, baud, name));
        }
    }
    lines.join("\n")
}

/// Core reader — returns timestamped lines. Used by both single and multi-port tools.
fn do_read_serial_log_raw(
    port: &str,
    baud: u32,
    max_lines: usize,
    timeout_s: u64,
) -> Result<Vec<(SystemTime, String)>> {
    let serial = serialport::new(port, baud)
        .timeout(Duration::from_secs(1))
        .open()
        .map_err(|e| anyhow::anyhow!("Could not open {}: {}", port, e))?;

    let mut reader = BufReader::new(serial);
    let mut buffer: Vec<(SystemTime, String)> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(timeout_s);

    while Instant::now() < deadline && buffer.len() < max_lines {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim().to_string();
                if !trimmed.is_empty() {
                    buffer.push((SystemTime::now(), trimmed));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
            Err(_) => break,
        }
    }

    Ok(buffer)
}

fn do_read_serial_log(port: &str, baud: u32, lines: usize, timeout_s: u64, timestamps: bool) -> Result<String> {
    let raw = do_read_serial_log_raw(port, baud, lines, timeout_s)?;

    if raw.is_empty() {
        return Ok("No data received. Check that the device is running and baud rate is correct.".to_string());
    }

    if timestamps {
        let formatted: Vec<String> = raw
            .iter()
            .map(|(ts, line)| format!("[{}]  {}", format_time_ms(*ts), line))
            .collect();
        Ok(flag_anomalies(&formatted))
    } else {
        let plain: Vec<String> = raw.into_iter().map(|(_, l)| l).collect();
        Ok(flag_anomalies(&plain))
    }
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

/// Read from multiple ports concurrently, merge output sorted by timestamp.
async fn do_read_multi_log(
    port_infos: Vec<(String, String, u32)>, // (label, path, baud)
    lines: usize,
    timeout_s: u64,
) -> String {
    let handles: Vec<_> = port_infos
        .into_iter()
        .map(|(label, path, baud)| {
            tokio::task::spawn_blocking(move || {
                let raw = do_read_serial_log_raw(&path, baud, lines, timeout_s)?;
                Ok::<_, anyhow::Error>((label, raw))
            })
        })
        .collect();

    let mut all: Vec<(SystemTime, String, String)> = Vec::new(); // (ts, label, line)
    let mut errors: Vec<String> = Vec::new();

    for handle in handles {
        match handle.await {
            Ok(Ok((label, timed_lines))) => {
                for (ts, line) in timed_lines {
                    all.push((ts, label.clone(), line));
                }
            }
            Ok(Err(e)) => errors.push(e.to_string()),
            Err(e) => errors.push(format!("task panic: {}", e)),
        }
    }

    if all.is_empty() && !errors.is_empty() {
        return format!("ERROR:\n{}", errors.join("\n"));
    }

    all.sort_by_key(|(ts, _, _)| *ts);

    let mut out_lines: Vec<String> = all
        .iter()
        .map(|(ts, label, line)| format!("[{:15}  {}]  {}", label, format_time_ms(*ts), line))
        .collect();

    if !errors.is_empty() {
        out_lines.push(format!("\nERRORS:\n{}", errors.join("\n")));
    }

    let plain: Vec<String> = all.into_iter().map(|(_, _, l)| l).collect();
    let anomaly_prefix = {
        let anomalies: Vec<&String> = plain
            .iter()
            .filter(|l| {
                let lower = l.to_lowercase();
                ANOMALY_KEYWORDS.iter().any(|k| lower.contains(k))
            })
            .collect();
        if anomalies.is_empty() {
            String::new()
        } else {
            format!(
                "WARNING: ANOMALY DETECTED:\n{}\n---\n",
                anomalies.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n")
            )
        }
    };

    anomaly_prefix + &out_lines.join("\n")
}

/// Poll multiple ports until `pattern` matches any line, or timeout.
/// `pattern` supports `|` as OR separator (case-insensitive substring match).
async fn do_wait_for_pattern(
    port_infos: Vec<(String, String, u32)>, // (label, path, baud)
    pattern: &str,
    timeout_s: u64,
) -> String {
    let keywords: Vec<String> = pattern
        .split('|')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();

    if keywords.is_empty() {
        return "ERROR: empty pattern.".to_string();
    }

    let deadline = Instant::now() + Duration::from_secs(timeout_s);

    loop {
        if Instant::now() >= deadline {
            return format!(
                "TIMEOUT: Pattern '{}' not matched on any port within {}s.",
                pattern, timeout_s
            );
        }

        // Poll each port with a short 1s window
        for (label, path, baud) in &port_infos {
            match do_read_serial_log_raw(path, *baud, 20, 1) {
                Ok(lines) => {
                    for (ts, line) in lines {
                        let lower = line.to_lowercase();
                        if keywords.iter().any(|k| lower.contains(k.as_str())) {
                            return format!(
                                "MATCH on '{}' at {}: {}",
                                label,
                                format_time_ms(ts),
                                line
                            );
                        }
                    }
                }
                Err(e) => {
                    // Port error — don't abort, just report and continue polling others
                    eprintln!("serial-mcp wait_for_pattern: {}: {}", label, e);
                }
            }
        }
    }
}

// ── MCP server ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct SerialMcp {
    tool_router: ToolRouter<Self>,
    registry: Arc<DeviceRegistry>,
}

impl SerialMcp {
    fn new(registry: DeviceRegistry) -> Self {
        Self {
            tool_router: Self::tool_router(),
            registry: Arc::new(registry),
        }
    }
}

// ── parameter structs ─────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ReadSerialLogParams {
    /// Serial port path (e.g. /dev/cu.usbmodem1303) or board alias (e.g. ping/log)
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
    /// Prefix each line with a wall-clock timestamp (default: false)
    #[serde(default)]
    timestamps: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SendCommandParams {
    /// Serial port path or board alias (e.g. ping/shell)
    port: String,
    /// Command string to send (will be sent with \r\n terminator)
    command: String,
    /// Baud rate (default: 115200)
    #[serde(default = "default_baud")]
    baud: u32,
    /// Number of response lines to collect (default: 10)
    #[serde(default = "default_response_lines")]
    response_lines: usize,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ReadMultiLogParams {
    /// Comma-separated ports or board aliases (e.g. "board1/log,board2/log" or "/dev/cu.usbmodem11201,/dev/cu.usbmodem11401")
    ports: String,
    /// Maximum lines to collect per port (default: 30)
    #[serde(default = "default_multi_lines", deserialize_with = "de_u64_or_str")]
    lines: u64,
    /// Read window in seconds per port (default: 8)
    #[serde(default = "default_timeout", deserialize_with = "de_u64_or_str")]
    timeout_s: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct WaitForPatternParams {
    /// Comma-separated ports or board aliases to monitor (e.g. "board1/log,board2/log")
    ports: String,
    /// Pattern to wait for. Supports | as OR separator (e.g. "HardFault|panic|assert")
    pattern: String,
    /// Maximum wait time in seconds (default: 30)
    #[serde(default = "default_wait_timeout", deserialize_with = "de_u64_or_str")]
    timeout_s: u64,
}

fn default_baud() -> u32 { 115200 }
fn default_lines() -> usize { 50 }
fn default_multi_lines() -> u64 { 30 }
fn default_timeout() -> u64 { 8 }
fn default_response_lines() -> usize { 10 }
fn default_wait_timeout() -> u64 { 30 }

/// Deserialize a u64 from either a JSON number or a JSON string (e.g. "30").
fn de_u64_or_str<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    use serde::de::{self, Visitor};
    struct V;
    impl<'de> Visitor<'de> for V {
        type Value = u64;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            write!(f, "a u64 or a string containing a u64")
        }
        fn visit_u64<E: de::Error>(self, v: u64) -> Result<u64, E> { Ok(v) }
        fn visit_i64<E: de::Error>(self, v: i64) -> Result<u64, E> {
            u64::try_from(v).map_err(de::Error::custom)
        }
        fn visit_str<E: de::Error>(self, v: &str) -> Result<u64, E> {
            v.trim().parse::<u64>().map_err(de::Error::custom)
        }
    }
    d.deserialize_any(V)
}

// ── tool implementations ──────────────────────────────────────────────────────

#[tool_router]
impl SerialMcp {
    #[tool(description = "List all available serial ports on this machine, including USB serial numbers.")]
    async fn list_serial_ports(&self) -> String {
        do_list_serial_ports().unwrap_or_else(|e| format!("ERROR: {}", e))
    }

    #[tool(description = "List all boards configured in devices.toml, showing their port aliases and paths.")]
    async fn list_boards(&self) -> String {
        do_list_boards(&self.registry)
    }

    #[tool(description = "Read up to N lines from a serial port. Accepts a raw port path or a board alias (e.g. 'ping/log'). Auto-flags anomalies like HardFault, Panic, Watchdog Reset.")]
    async fn read_serial_log(&self, Parameters(p): Parameters<ReadSerialLogParams>) -> String {
        let (path, baud, _label) = match resolve_port(&p.port, &self.registry) {
            Ok(r) => r,
            Err(e) => return format!("ERROR: {}", e),
        };
        // Explicit baud param overrides alias config baud (raw paths always use param)
        let effective_baud = if p.port.contains('/') && self.registry.boards.contains_key(p.port.split('/').next().unwrap_or("")) {
            baud // from config
        } else {
            p.baud // from param
        };
        do_read_serial_log(&path, effective_baud, p.lines, p.timeout_s, p.timestamps)
            .unwrap_or_else(|e| format!("ERROR: {}", e))
    }

    #[tool(description = "Send a command over serial and return the response. Accepts a raw port path or a board alias (e.g. 'ping/shell').")]
    async fn send_serial_command(&self, Parameters(p): Parameters<SendCommandParams>) -> String {
        let (path, baud, _label) = match resolve_port(&p.port, &self.registry) {
            Ok(r) => r,
            Err(e) => return format!("ERROR: {}", e),
        };
        let effective_baud = if p.port.contains('/') && self.registry.boards.contains_key(p.port.split('/').next().unwrap_or("")) {
            baud
        } else {
            p.baud
        };
        do_send_serial_command(&path, &p.command, effective_baud, p.response_lines)
            .unwrap_or_else(|e| format!("ERROR: {}", e))
    }

    #[tool(description = "Read from multiple serial ports concurrently and return interleaved, timestamped output. Ideal for monitoring multiple boards simultaneously (e.g. LoRa ping-pong). Pass ports as a comma-separated string of raw paths or board aliases, e.g. 'board1/log,board2/log'.")]
    async fn read_multi_log(&self, Parameters(p): Parameters<ReadMultiLogParams>) -> String {
        let aliases: Vec<&str> = p.ports.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        if aliases.is_empty() {
            return "ERROR: no ports specified.".to_string();
        }
        let mut port_infos = Vec::new();
        for alias in aliases {
            match resolve_port(alias, &self.registry) {
                Ok((path, baud, label)) => port_infos.push((label, path, baud)),
                Err(e) => return format!("ERROR resolving '{}': {}", alias, e),
            }
        }
        do_read_multi_log(port_infos, p.lines as usize, p.timeout_s).await
    }

    #[tool(description = "Monitor one or more serial ports and return as soon as any line matches the pattern. Pattern supports | as OR (e.g. 'HardFault|panic'). Pass ports as a comma-separated string, e.g. 'board1/log,board2/log'. Useful in test loops to detect the first fault across multiple boards.")]
    async fn wait_for_pattern(&self, Parameters(p): Parameters<WaitForPatternParams>) -> String {
        let aliases: Vec<&str> = p.ports.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        if aliases.is_empty() {
            return "ERROR: no ports specified.".to_string();
        }
        let mut port_infos = Vec::new();
        for alias in aliases {
            match resolve_port(alias, &self.registry) {
                Ok((path, baud, label)) => port_infos.push((label, path, baud)),
                Err(e) => return format!("ERROR resolving '{}': {}", alias, e),
            }
        }
        do_wait_for_pattern(port_infos, &p.pattern, p.timeout_s).await
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
    let registry = load_registry();
    let server = SerialMcp::new(registry);
    let transport = stdio();
    let handle = server.serve(transport).await?;
    handle.waiting().await?;
    Ok(())
}
