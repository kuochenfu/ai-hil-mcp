use anyhow::Result;
use ppk2::{
    measurement::MeasurementMatch,
    types::{DevicePower, Level, LogicPortPins, MeasurementMode, SourceVoltage},
    Ppk2,
};
use std::sync::mpsc::Receiver;
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::Deserialize;
use std::{
    sync::{Arc, Mutex, mpsc::{self, SyncSender}},
    thread,
    time::{Duration, Instant},
};

// Spike detection: flag if peak exceeds average by this ratio
const SPIKE_RATIO_THRESHOLD: f32 = 10.0;
// Samples per second for measurements (1kHz = 1ms resolution)
const MEASUREMENT_SPS: usize = 1_000;
// Retries for PPK2 init (device sometimes needs settling time)
const INIT_RETRIES: u32 = 3;
const INIT_RETRY_DELAY: Duration = Duration::from_millis(600);

// Current histogram bucket boundaries in µA (log-scale decades)
const BUCKETS: &[(&str, f32, f32)] = &[
    ("< 1 µA    (deep sleep)", f32::NEG_INFINITY, 1.0),
    ("1–10 µA   (sleep)",      1.0,               10.0),
    ("10–100 µA (idle)",       10.0,              100.0),
    ("100µA–1mA (light load)", 100.0,             1_000.0),
    ("1–10 mA   (active)",     1_000.0,           10_000.0),
    ("> 10 mA   (TX / peak)",  10_000.0,          f32::INFINITY),
];

// --- Persistent power hold (background thread keeps PPK2 connection open) ---

/// Dropping this struct signals the background thread to stop and power off the DUT.
#[derive(Debug)]
struct PowerHold {
    #[allow(dead_code)] // intentionally held for its Drop side-effect (signals background thread)
    stop_tx: SyncSender<()>,
}

// --- Helpers ---

fn open_ppk2(port: &str, mode: MeasurementMode) -> Result<Ppk2> {
    let mut last_err = anyhow::anyhow!("No attempts made");
    for attempt in 1..=INIT_RETRIES {
        match Ppk2::new(port, mode) {
            Ok(ppk2) => return Ok(ppk2),
            Err(e) => {
                last_err = anyhow::anyhow!("{}", e);
                if attempt < INIT_RETRIES {
                    thread::sleep(INIT_RETRY_DELAY);
                }
            }
        }
    }
    Err(last_err)
}

struct Stats {
    count: usize,
    min: f32,
    max: f32,
    avg: f32,
    std_dev: f32,
    p50: f32,
    p95: f32,
    p99: f32,
}

fn compute_stats(samples: &mut Vec<f32>) -> Stats {
    let count = samples.len();
    let min = samples.iter().cloned().fold(f32::INFINITY, f32::min);
    let max = samples.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let avg = samples.iter().sum::<f32>() / count as f32;
    let variance = samples.iter().map(|&x| (x - avg).powi(2)).sum::<f32>() / count as f32;
    let std_dev = variance.sqrt();

    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p50 = samples[count * 50 / 100];
    let p95 = samples[count * 95 / 100];
    let p99 = samples[count * 99 / 100];

    Stats { count, min, max, avg, std_dev, p50, p95, p99 }
}

fn format_ua(ua: f32) -> String {
    let abs = ua.abs();
    if abs >= 1_000.0 {
        format!("{:.2} mA", ua / 1_000.0)
    } else {
        format!("{:.2} µA", ua)
    }
}

fn spike_warning(max: f32, avg: f32) -> Option<String> {
    if max > avg * SPIKE_RATIO_THRESHOLD {
        Some(format!(
            "WARNING: CURRENT SPIKE DETECTED — peak {} exceeds {:.0}× average ({})\n",
            format_ua(max), SPIKE_RATIO_THRESHOLD, format_ua(avg)
        ))
    } else {
        None
    }
}

fn setup_and_start(
    ppk2: Ppk2,
    mmode: MeasurementMode,
    voltage_mv: u16,
) -> Result<(Receiver<MeasurementMatch>, impl FnOnce() -> ppk2::Result<Ppk2>)>
where {
    setup_and_start_with_pins(ppk2, mmode, voltage_mv, LogicPortPins::default())
}

fn setup_and_start_with_pins(
    mut ppk2: Ppk2,
    mmode: MeasurementMode,
    voltage_mv: u16,
    pins: LogicPortPins,
) -> Result<(Receiver<MeasurementMatch>, impl FnOnce() -> ppk2::Result<Ppk2>)>
{
    if matches!(mmode, MeasurementMode::Source) {
        ppk2.set_source_voltage(SourceVoltage::from_millivolts(voltage_mv))
            .map_err(|e| anyhow::anyhow!("{}", e))?;
    }
    ppk2.set_device_power(DevicePower::Enabled)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let (rx, stop) = ppk2
        .start_measurement_matching(pins, MEASUREMENT_SPS)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    Ok((rx, stop))
}

fn collect_samples(
    rx: &Receiver<MeasurementMatch>,
    duration_s: f32,
) -> (Vec<f32>, usize) {
    let deadline = Instant::now() + Duration::from_secs_f32(duration_s);
    let mut samples: Vec<f32> = Vec::new();
    let mut no_match_count: usize = 0;
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(MeasurementMatch::Match(m)) => samples.push(m.micro_amps),
            Ok(MeasurementMatch::NoMatch) => no_match_count += 1,
            Err(_) => break,
        }
    }
    (samples, no_match_count)
}

fn stop_and_power_off(stop: impl FnOnce() -> ppk2::Result<Ppk2>) -> Result<()> {
    let mut ppk2 = stop().map_err(|e| anyhow::anyhow!("{}", e))?;
    ppk2.set_device_power(DevicePower::Disabled)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    Ok(())
}

// --- Measurement implementations ---

fn do_find_ppk2() -> Result<String> {
    use serialport::SerialPortType::UsbPort;

    let mut ports: Vec<String> = serialport::available_ports()
        .map_err(|e| anyhow::anyhow!("{}", e))?
        .into_iter()
        .filter(|p| matches!(&p.port_type, UsbPort(u) if u.vid == 0x1915 && u.pid == 0xc00a))
        .map(|p| p.port_name)
        .collect();

    if ports.is_empty() {
        return Err(anyhow::anyhow!("PPK2 not found. Is the device connected?"));
    }

    ports.sort();
    let control_port = &ports[0];

    if ports.len() > 1 {
        Ok(format!(
            "PPK2 found. Control port: {} (data port: {})\nUse: {}",
            control_port,
            ports[1..].join(", "),
            control_port
        ))
    } else {
        Ok(format!("PPK2 found at: {}", control_port))
    }
}

fn do_measure_current(port: &str, mode: &str, voltage_mv: u16, duration_s: f32) -> Result<String> {
    let mmode = parse_mode(mode)?;
    let ppk2 = open_ppk2(port, mmode)?;
    let (rx, stop) = setup_and_start(ppk2, mmode, voltage_mv)?;
    let (mut samples, _) = collect_samples(&rx, duration_s);
    stop_and_power_off(stop)?;

    if samples.is_empty() {
        return Ok("No samples collected. Check PPK2 connection and DUT.".to_string());
    }

    let s = compute_stats(&mut samples);
    let voltage_v = if matches!(mmode, MeasurementMode::Source) { voltage_mv as f32 / 1000.0 } else { 0.0 };
    let energy_uj = s.avg * voltage_v * duration_s;

    let mut out = format!(
        "Measurement complete ({:.1}s, {} samples)\n\
         Mode:       {}\n\
         Min:        {}\n\
         Max:        {}\n\
         Avg:        {}\n\
         Std dev:    {}\n\
         p50:        {}\n\
         p95:        {}\n\
         p99:        {}\n\
         Peak/Avg:   {:.1}x",
        duration_s, s.count, mode,
        format_ua(s.min), format_ua(s.max), format_ua(s.avg), format_ua(s.std_dev),
        format_ua(s.p50), format_ua(s.p95), format_ua(s.p99),
        s.max / s.avg.max(0.001)
    );

    if matches!(mmode, MeasurementMode::Source) {
        out.push_str(&format!(
            "\nVoltage:    {} mV\nEnergy:     {:.2} µJ  ({:.4} mWh)",
            voltage_mv, energy_uj, energy_uj / 3_600_000.0
        ));
    }

    if let Some(w) = spike_warning(s.max, s.avg) {
        out = format!("{}{}", w, out);
    }

    Ok(out)
}

fn do_profile_power_states(port: &str, mode: &str, voltage_mv: u16, duration_s: f32) -> Result<String> {
    let mmode = parse_mode(mode)?;
    let ppk2 = open_ppk2(port, mmode)?;
    let (rx, stop) = setup_and_start(ppk2, mmode, voltage_mv)?;
    let (mut samples, _) = collect_samples(&rx, duration_s);
    stop_and_power_off(stop)?;

    if samples.is_empty() {
        return Ok("No samples collected.".to_string());
    }

    let s = compute_stats(&mut samples);
    let total = s.count as f32;

    let mut out = format!(
        "Power State Profile ({:.1}s, {} samples)\n\
         {}\n\
         {:<28} {:>8}  {:>6}  {:>12}\n\
         {}",
        duration_s, s.count,
        "─".repeat(58),
        "Range", "Samples", "Time%", "Avg (µA)",
        "─".repeat(58),
    );

    let mut dominant_label = "";
    let mut dominant_pct = 0.0f32;
    let mut sleep_avg: Option<f32> = None;
    let mut active_avg: Option<f32> = None;

    for &(label, lo, hi) in BUCKETS {
        let bucket: Vec<f32> = samples.iter()
            .cloned()
            .filter(|&x| x >= lo && x < hi)
            .collect();
        let count = bucket.len();
        if count == 0 {
            out.push_str(&format!("\n{:<28} {:>8}  {:>5.1}%  {:>12}", label, 0, 0.0, "—"));
            continue;
        }
        let pct = count as f32 / total * 100.0;
        let avg = bucket.iter().sum::<f32>() / count as f32;
        out.push_str(&format!(
            "\n{:<28} {:>8}  {:>5.1}%  {:>12}",
            label, count, pct, format_ua(avg)
        ));
        if pct > dominant_pct {
            dominant_pct = pct;
            dominant_label = label;
        }
        // lowest occupied bucket = sleep estimate
        if sleep_avg.is_none() { sleep_avg = Some(avg); }
        // highest occupied bucket = active estimate
        active_avg = Some(avg);
    }

    out.push_str(&format!("\n{}", "─".repeat(58)));
    out.push_str(&format!("\nOverall avg:  {}  |  std dev: {}", format_ua(s.avg), format_ua(s.std_dev)));
    out.push_str(&format!("\nDominant state: {} ({:.1}%)", dominant_label.trim(), dominant_pct));
    if let (Some(sl), Some(ac)) = (sleep_avg, active_avg) {
        if (ac - sl).abs() > 10.0 {
            out.push_str(&format!(
                "\nEst. sleep current:  {}", format_ua(sl)
            ));
            out.push_str(&format!(
                "\nEst. active current: {}", format_ua(ac)
            ));
        }
    }

    Ok(out)
}

fn do_measure_with_pin_trigger(
    port: &str,
    mode: &str,
    voltage_mv: u16,
    duration_s: f32,
    pin: u8,
    trigger_level: &str,
) -> Result<String> {
    if pin > 7 {
        return Err(anyhow::anyhow!("Pin must be 0–7, got {}", pin));
    }

    let level = match trigger_level.to_lowercase().as_str() {
        "high" | "h" | "1" => Level::High,
        "low"  | "l" | "0" => Level::Low,
        _ => return Err(anyhow::anyhow!("trigger_level must be 'high' or 'low', got '{}'", trigger_level)),
    };

    let mut pin_levels = [Level::Either; 8];
    pin_levels[pin as usize] = level;
    let pins = LogicPortPins::with_levels(pin_levels);

    let mmode = parse_mode(mode)?;
    let ppk2 = open_ppk2(port, mmode)?;
    let (rx, stop) = setup_and_start_with_pins(ppk2, mmode, voltage_mv, pins)?;
    let (mut matched_samples, no_match) = collect_samples(&rx, duration_s);
    stop_and_power_off(stop)?;

    let total_samples = matched_samples.len() + no_match;
    let match_pct = if total_samples > 0 {
        matched_samples.len() as f32 / total_samples as f32 * 100.0
    } else { 0.0 };

    if matched_samples.is_empty() {
        return Ok(format!(
            "No matching samples for pin {} = {} during {:.1}s.\n\
             Total samples seen: {}. Is the pin toggling?",
            pin, trigger_level, duration_s, total_samples
        ));
    }

    let s = compute_stats(&mut matched_samples);

    Ok(format!(
        "Pin-triggered Measurement (pin {} = {}, {:.1}s)\n\
         Matched samples:  {} / {} ({:.1}% of time pin was {})\n\
         {}\n\
         Min:     {}\n\
         Max:     {}\n\
         Avg:     {}\n\
         Std dev: {}\n\
         p50:     {}  p95: {}  p99: {}",
        pin, trigger_level, duration_s,
        s.count, total_samples, match_pct, trigger_level,
        "─".repeat(40),
        format_ua(s.min), format_ua(s.max), format_ua(s.avg), format_ua(s.std_dev),
        format_ua(s.p50), format_ua(s.p95), format_ua(s.p99),
    ))
}

fn do_estimate_battery_life(
    port: &str,
    mode: &str,
    voltage_mv: u16,
    duration_s: f32,
    battery_capacity_mah: f32,
) -> Result<String> {
    let mmode = parse_mode(mode)?;
    let ppk2 = open_ppk2(port, mmode)?;
    let (rx, stop) = setup_and_start(ppk2, mmode, voltage_mv)?;
    let (mut samples, _) = collect_samples(&rx, duration_s);
    stop_and_power_off(stop)?;

    if samples.is_empty() {
        return Ok("No samples collected.".to_string());
    }

    let s = compute_stats(&mut samples);
    let avg_ma = s.avg / 1_000.0;
    let avg_mw = avg_ma * (voltage_mv as f32 / 1000.0);
    let runtime_h = battery_capacity_mah / avg_ma.max(0.001);
    let runtime_days = runtime_h / 24.0;
    let energy_mwh = battery_capacity_mah * (voltage_mv as f32 / 1000.0);

    Ok(format!(
        "Battery Life Estimate\n\
         {}\n\
         Measured avg current: {}  (p95: {})\n\
         Supply voltage:       {} mV\n\
         Average power:        {:.3} mW\n\
         Battery capacity:     {:.0} mAh  ({:.1} mWh)\n\
         {}\n\
         Estimated runtime:    {:.1} hours  ({:.1} days)\n\
         Energy per second:    {:.2} µJ\n\
         {}",
        "─".repeat(50),
        format_ua(s.avg), format_ua(s.p95),
        voltage_mv,
        avg_mw,
        battery_capacity_mah, energy_mwh,
        "─".repeat(50),
        runtime_h, runtime_days,
        s.avg * (voltage_mv as f32 / 1000.0),
        "─".repeat(50),
    ))
}

fn do_get_metadata(port: &str) -> Result<String> {
    let mut ppk2 = open_ppk2(port, MeasurementMode::Source)?;
    let meta = ppk2.get_metadata().map_err(|e| anyhow::anyhow!("{}", e))?;

    Ok(format!(
        "PPK2 Metadata\n\
         Calibrated: {}\n\
         VDD:        {} mV\n\
         HW version: {}\n\
         Mode:       {:?}\n\
         IA:         {}",
        meta.calibrated, meta.vdd, meta.hw, meta.mode, meta.ia
    ))
}

fn parse_mode(mode: &str) -> Result<MeasurementMode> {
    match mode.to_lowercase().as_str() {
        "source_meter" | "source" | "s" => Ok(MeasurementMode::Source),
        "ampere_meter" | "ampere" | "amp" | "a" => Ok(MeasurementMode::Ampere),
        _ => Err(anyhow::anyhow!(
            "Unknown mode '{}'. Use 'source_meter' or 'ampere_meter'.",
            mode
        )),
    }
}

// --- Parameter structs ---

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct MeasureParams {
    /// Serial port path (from find_ppk2, e.g. /dev/cu.usbmodem1301)
    port: String,
    /// Measurement mode: "source_meter" (PPK2 supplies voltage) or "ampere_meter" (external supply)
    #[serde(default = "default_mode")]
    mode: String,
    /// Supply voltage in millivolts, 800–5000 (only used in source_meter mode, default: 3300)
    #[serde(default = "default_voltage_mv")]
    voltage_mv: u16,
    /// Measurement duration in seconds (default: 3.0)
    #[serde(default = "default_duration_s")]
    duration_s: f32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct PinTriggerParams {
    /// Serial port path (from find_ppk2)
    port: String,
    /// Measurement mode: "source_meter" or "ampere_meter"
    #[serde(default = "default_mode")]
    mode: String,
    /// Supply voltage in millivolts (default: 3300)
    #[serde(default = "default_voltage_mv")]
    voltage_mv: u16,
    /// Measurement duration in seconds (default: 3.0)
    #[serde(default = "default_duration_s")]
    duration_s: f32,
    /// Logic input pin to filter on (0–7)
    pin: u8,
    /// Pin level to match: "high" or "low"
    trigger_level: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct BatteryParams {
    /// Serial port path (from find_ppk2)
    port: String,
    /// Measurement mode: "source_meter" or "ampere_meter"
    #[serde(default = "default_mode")]
    mode: String,
    /// Supply voltage in millivolts (default: 3300)
    #[serde(default = "default_voltage_mv")]
    voltage_mv: u16,
    /// Measurement duration in seconds (default: 5.0)
    #[serde(default = "default_battery_duration_s")]
    duration_s: f32,
    /// Battery capacity in mAh (e.g. 2000 for a 2000 mAh LiPo)
    battery_capacity_mah: f32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DutPowerParams {
    /// Serial port path (from find_ppk2)
    port: String,
    /// Measurement mode: "source_meter" or "ampere_meter"
    #[serde(default = "default_mode")]
    mode: String,
    /// Supply voltage in millivolts (only used in source_meter mode, default: 3300)
    #[serde(default = "default_voltage_mv")]
    voltage_mv: u16,
    /// Enable (true) or disable (false) DUT power
    enabled: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct PortParams {
    /// Serial port path (from find_ppk2)
    port: String,
}

fn default_mode() -> String { "source_meter".to_string() }
fn default_voltage_mv() -> u16 { 3300 }
fn default_duration_s() -> f32 { 3.0 }
fn default_battery_duration_s() -> f32 { 5.0 }

// --- MCP server ---

#[derive(Debug, Clone)]
struct Ppk2Server {
    tool_router: ToolRouter<Self>,
    /// Holds the background thread that keeps the PPK2 connection open for set_dut_power(enabled=true).
    /// Dropping the PowerHold sends a stop signal to the thread, which then powers off the DUT.
    power_hold: Arc<Mutex<Option<PowerHold>>>,
}

impl Ppk2Server {
    fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
            power_hold: Arc::new(Mutex::new(None)),
        }
    }
}

#[tool_router]
impl Ppk2Server {
    #[tool(description = "Auto-detect the PPK2 serial port. Returns the correct control port path to use with all other tools. PPK2 exposes two CDC ports on macOS — this picks the right one.")]
    async fn find_ppk2(&self) -> String {
        do_find_ppk2().unwrap_or_else(|e| format!("ERROR: {}", e))
    }

    #[tool(description = "Measure current draw for a specified duration. Returns min/max/avg/std-dev/percentiles (p50,p95,p99), energy in µJ and mWh, and spike detection. DUT power is automatically enabled before and disabled after.")]
    async fn measure_current(&self, Parameters(p): Parameters<MeasureParams>) -> String {
        do_measure_current(&p.port, &p.mode, p.voltage_mv, p.duration_s)
            .unwrap_or_else(|e| format!("ERROR: {}", e))
    }

    #[tool(description = "Profile power states: measures current and shows a histogram across log-scale current bands (deep sleep / sleep / idle / light-load / active / TX). Identifies dominant state and estimates sleep vs active current levels.")]
    async fn profile_power_states(&self, Parameters(p): Parameters<MeasureParams>) -> String {
        do_profile_power_states(&p.port, &p.mode, p.voltage_mv, p.duration_s)
            .unwrap_or_else(|e| format!("ERROR: {}", e))
    }

    #[tool(description = "Measure current only when a specific logic input pin (0–7) is HIGH or LOW. Useful for correlating firmware activity (GPIO toggles, events) with power consumption. Returns stats for matched samples and the % of time the trigger was active.")]
    async fn measure_with_pin_trigger(&self, Parameters(p): Parameters<PinTriggerParams>) -> String {
        do_measure_with_pin_trigger(&p.port, &p.mode, p.voltage_mv, p.duration_s, p.pin, &p.trigger_level)
            .unwrap_or_else(|e| format!("ERROR: {}", e))
    }

    #[tool(description = "Estimate battery life based on a real current measurement. Provide battery capacity in mAh; returns estimated runtime in hours and days, average power in mW, and energy per second.")]
    async fn estimate_battery_life(&self, Parameters(p): Parameters<BatteryParams>) -> String {
        do_estimate_battery_life(&p.port, &p.mode, p.voltage_mv, p.duration_s, p.battery_capacity_mah)
            .unwrap_or_else(|e| format!("ERROR: {}", e))
    }

    #[tool(description = "Enable or disable DUT power via PPK2 without running a measurement. enabled=true spawns a background thread that holds the PPK2 connection open (PPK2 cuts power when connection closes). enabled=false signals that thread to stop, which powers off the DUT and closes the connection.")]
    async fn set_dut_power(&self, Parameters(p): Parameters<DutPowerParams>) -> String {
        let power_hold = Arc::clone(&self.power_hold);

        if p.enabled {
            let port = p.port.clone();
            let mode_str = p.mode.clone();
            let voltage_mv = p.voltage_mv;

            // Open PPK2 and enable power in a blocking thread (serial I/O is sync).
            // On success, move the Ppk2 handle into a hold thread that blocks until stop signal.
            let result = tokio::task::spawn_blocking(move || -> Result<SyncSender<()>, String> {
                let mmode = parse_mode(&mode_str).map_err(|e| e.to_string())?;
                let mut ppk2 = open_ppk2(&port, mmode).map_err(|e| e.to_string())?;

                if matches!(mmode, MeasurementMode::Source) {
                    ppk2.set_source_voltage(SourceVoltage::from_millivolts(voltage_mv))
                        .map_err(|e| e.to_string())?;
                }
                ppk2.set_device_power(DevicePower::Enabled)
                    .map_err(|e| e.to_string())?;

                // Spawn the hold thread. It owns the Ppk2 handle and keeps the serial
                // connection open. Dropping stop_tx causes stop_rx.recv() to return Err,
                // which unblocks the thread → powers off DUT → port closes.
                let (stop_tx, stop_rx) = mpsc::sync_channel::<()>(1);
                thread::spawn(move || {
                    let _ = stop_rx.recv(); // blocks until PowerHold is dropped
                    let _ = ppk2.set_device_power(DevicePower::Disabled);
                    // ppk2 drops here, serial port closes
                });

                Ok(stop_tx)
            }).await;

            match result {
                Ok(Ok(stop_tx)) => {
                    *power_hold.lock().unwrap() = Some(PowerHold { stop_tx });
                    if matches!(parse_mode(&p.mode).unwrap_or(MeasurementMode::Source), MeasurementMode::Source) {
                        format!("DUT power ON: {} mV (source meter mode)\nBackground thread holding connection open.", p.voltage_mv)
                    } else {
                        "DUT power ON: ampere meter mode (external supply)\nBackground thread holding connection open.".to_string()
                    }
                }
                Ok(Err(e)) => format!("ERROR: {}", e),
                Err(e) => format!("ERROR: task failed: {}", e),
            }
        } else {
            // Drop the PowerHold: this drops stop_tx, unblocking the hold thread,
            // which calls set_device_power(Disabled) before the port closes.
            let had_hold = {
                let mut guard = power_hold.lock().unwrap();
                let had = guard.is_some();
                *guard = None; // drops PowerHold → drops stop_tx → thread exits
                had
            };

            if !had_hold {
                // No background thread was running — directly power off.
                let port = p.port.clone();
                let mode_str = p.mode.clone();
                let voltage_mv = p.voltage_mv;
                let _ = tokio::task::spawn_blocking(move || {
                    if let Ok(mmode) = parse_mode(&mode_str) {
                        if let Ok(mut ppk2) = open_ppk2(&port, mmode) {
                            if matches!(mmode, MeasurementMode::Source) {
                                let _ = ppk2.set_source_voltage(SourceVoltage::from_millivolts(voltage_mv));
                            }
                            let _ = ppk2.set_device_power(DevicePower::Disabled);
                        }
                    }
                }).await;
            }

            if matches!(parse_mode(&p.mode).unwrap_or(MeasurementMode::Source), MeasurementMode::Source) {
                format!("DUT power OFF: {} mV (source meter mode)", p.voltage_mv)
            } else {
                "DUT power OFF: ampere meter mode (external supply)".to_string()
            }
        }
    }

    #[tool(description = "Read PPK2 device metadata: calibration status, current VDD setting, hardware version, and measurement mode.")]
    async fn get_metadata(&self, Parameters(p): Parameters<PortParams>) -> String {
        do_get_metadata(&p.port).unwrap_or_else(|e| format!("ERROR: {}", e))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Ppk2Server {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let server = Ppk2Server::new();
    let transport = stdio();
    let handle = server.serve(transport).await?;
    handle.waiting().await?;
    Ok(())
}
