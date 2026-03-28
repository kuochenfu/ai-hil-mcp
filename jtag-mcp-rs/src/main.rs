use anyhow::{Context, Result};
use probe_rs::{
    architecture::arm::core::registers::cortex_m::XPSR,
    probe::list::Lister,
    MemoryInterface, Permissions, Session,
};
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::Deserialize;
use std::{collections::HashMap, sync::Arc};

// ── device registry ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Default)]
struct BoardConfig {
    probe_serial: Option<String>,
    target: Option<String>,
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
    let path = std::env::var("JTAG_MCP_CONFIG")
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
        Err(e) => { eprintln!("jtag-mcp: could not read {:?}: {}", path, e); return DeviceRegistry::default(); }
    };

    let raw: RawRegistry = match toml::from_str(&content) {
        Ok(r) => r,
        Err(e) => { eprintln!("jtag-mcp: could not parse {:?}: {}", path, e); return DeviceRegistry::default(); }
    };

    DeviceRegistry { boards: raw.board }
}

/// Resolve board alias → (probe_serial, target, label).
/// If board is None, falls back to first probe + default_target.
fn resolve_board(
    board: Option<&str>,
    registry: &DeviceRegistry,
    default_target: &str,
) -> Result<(Option<String>, String, String)> {
    match board {
        None => Ok((None, default_target.to_string(), "default".to_string())),
        Some(alias) => {
            let cfg = registry.boards.get(alias).ok_or_else(|| {
                let known: Vec<_> = registry.boards.keys().collect();
                anyhow::anyhow!(
                    "Unknown board '{}'. Configured: [{}]. Run list_probes to see available probes.",
                    alias,
                    known.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
                )
            })?;
            let target = cfg.target.clone().unwrap_or_else(|| default_target.to_string());
            Ok((cfg.probe_serial.clone(), target, alias.to_string()))
        }
    }
}

/// Returns true if target is a Cortex-M architecture.
/// ESP32 (Xtensa/RISC-V), RISC-V MCUs are not Cortex-M.
fn is_cortex_m(target: &str) -> bool {
    let t = target.to_lowercase();
    !t.starts_with("esp32") && !t.contains("riscv") && !t.contains("rv32") && !t.contains("xtensa")
}

fn open_session(probe_serial: Option<&str>, target: &str) -> Result<Session> {
    let lister = Lister::new();
    let probes = lister.list_all();

    if probes.is_empty() {
        anyhow::bail!("No debug probe found. Is a J-Link/ST-Link/ESP JTAG connected?");
    }

    let probe_info = match probe_serial {
        Some(serial) => probes
            .into_iter()
            .find(|p| p.serial_number.as_deref() == Some(serial))
            .with_context(|| format!(
                "No probe with serial '{}' found. Run list_probes to see connected probes.",
                serial
            ))?,
        None => probes.into_iter().next().unwrap(),
    };

    let probe = probe_info.open().context("Failed to open debug probe")?;
    probe
        .attach(target, Permissions::default())
        .with_context(|| format!("Failed to attach to target '{}'. Check target name and probe connection.", target))
}

// ── Cortex-M constants ────────────────────────────────────────────────────────

const CFSR: u64 = 0xE000_ED28;
const HFSR: u64 = 0xE000_ED2C;
const BFAR: u64 = 0xE000_ED38;
const MMFAR: u64 = 0xE000_ED34;
const DEMCR: u64 = 0xE000_EDFC;
const FP_CTRL: u64 = 0xE000_2000;
const FP_COMP_BASE: u64 = 0xE000_2008;
const DWT_CTRL: u64 = 0xE000_1000;
const DWT_COMP_BASE: u64 = 0xE000_1020;
const DWT_MAX_COMPARATORS: usize = 4;

fn dwt_comp(u: usize) -> u64 { DWT_COMP_BASE + (u as u64) * 0x10 }
fn dwt_mask(u: usize) -> u64 { DWT_COMP_BASE + (u as u64) * 0x10 + 0x4 }
fn dwt_func(u: usize) -> u64 { DWT_COMP_BASE + (u as u64) * 0x10 + 0x8 }

const DWT_FUNC_DISABLED: u32   = 0b0000;
const DWT_FUNC_READ: u32       = 0b0101;
const DWT_FUNC_WRITE: u32      = 0b0110;
const DWT_FUNC_READ_WRITE: u32 = 0b0111;

const EXC_FRAME: &[(u32, &str)] = &[
    (0x00, "R0"), (0x04, "R1"), (0x08, "R2"), (0x0C, "R3"),
    (0x10, "R12"), (0x14, "LR"), (0x18, "PC"), (0x1C, "xPSR"),
];

const CFSR_BITS: &[(u32, &str)] = &[
    (0,  "IACCVIOL — Instruction access violation"),
    (1,  "DACCVIOL — Data access violation"),
    (3,  "MUNSTKERR — MemManage fault on exception return unstacking"),
    (4,  "MSTKERR — MemManage fault on exception entry stacking"),
    (5,  "MLSPERR — MemManage fault during FP lazy state preservation"),
    (7,  "MMARVALID — MMFAR holds valid fault address"),
    (8,  "IBUSERR — Instruction bus error"),
    (9,  "PRECISERR — Precise data bus error (BFAR is valid)"),
    (10, "IMPRECISERR — Imprecise data bus error"),
    (11, "UNSTKERR — Bus fault on exception return unstacking"),
    (12, "STKERR — Bus fault on exception entry stacking"),
    (13, "LSPERR — Bus fault during FP lazy state preservation"),
    (15, "BFARVALID — BFAR holds valid fault address"),
    (16, "UNDEFINSTR — Undefined instruction"),
    (17, "INVSTATE — Invalid EPSR state"),
    (18, "INVPC — Invalid EXC_RETURN"),
    (19, "NOCP — No coprocessor"),
    (24, "UNALIGNED — Unaligned memory access"),
    (25, "DIVBYZERO — Integer divide by zero"),
];

const HFSR_BITS: &[(u32, &str)] = &[
    (1,  "VECTTBL — HardFault on vector table read"),
    (30, "FORCED — Escalated from configurable fault (check CFSR)"),
    (31, "DEBUGEVT — Debug event triggered HardFault"),
];

// ── tool implementations ──────────────────────────────────────────────────────

fn do_list_probes() -> Result<String> {
    let lister = Lister::new();
    let probes = lister.list_all();
    if probes.is_empty() {
        return Ok("No debug probes found.".to_string());
    }
    let lines: Vec<String> = probes
        .iter()
        .enumerate()
        .map(|(i, p)| {
            format!(
                "[{}] {} — VID:{:04X} PID:{:04X} — serial: {}",
                i,
                p.identifier,
                p.vendor_id,
                p.product_id,
                p.serial_number.as_deref().unwrap_or("?")
            )
        })
        .collect();
    Ok(lines.join("\n"))
}

fn do_list_boards(registry: &DeviceRegistry, default_target: &str) -> String {
    if registry.boards.is_empty() {
        return "No boards configured. Set JTAG_MCP_CONFIG to a devices.toml path.".to_string();
    }
    let mut lines = vec!["Configured boards (JTAG):".to_string()];
    let mut names: Vec<_> = registry.boards.keys().collect();
    names.sort();
    for name in names {
        let b = &registry.boards[name];
        let target = b.target.as_deref().unwrap_or(default_target);
        let serial = b.probe_serial.as_deref().unwrap_or("(first available)");
        let desc = b.description.as_deref().unwrap_or("");
        let arch = if is_cortex_m(target) { "Cortex-M" } else { "non-Cortex-M (basic tools only)" };
        lines.push(format!("  {}  {}", name, desc));
        lines.push(format!("    target:       {}  ({})", target, arch));
        lines.push(format!("    probe_serial: {}", serial));
    }
    lines.join("\n")
}

fn do_halt_cpu(probe_serial: Option<&str>, target: &str) -> Result<String> {
    let mut session = open_session(probe_serial, target)?;
    let mut core = session.core(0)?;
    core.halt(std::time::Duration::from_millis(500))?;
    let pc: u32 = core.read_core_reg(core.program_counter())?;
    Ok(format!("CPU halted. PC = 0x{:08X}", pc))
}

fn do_resume_cpu(probe_serial: Option<&str>, target: &str) -> Result<String> {
    let mut session = open_session(probe_serial, target)?;
    let mut core = session.core(0)?;
    core.run()?;
    Ok("CPU resumed.".to_string())
}

fn do_reset_target(probe_serial: Option<&str>, target: &str) -> Result<String> {
    let mut session = open_session(probe_serial, target)?;
    let mut core = session.core(0)?;
    core.reset_and_halt(std::time::Duration::from_millis(500))?;
    let pc: u32 = core.read_core_reg(core.program_counter())?;
    core.run()?;
    Ok(format!("Target reset. PC after reset = 0x{:08X}. CPU resumed.", pc))
}

fn do_read_registers(probe_serial: Option<&str>, target: &str) -> Result<String> {
    let mut session = open_session(probe_serial, target)?;
    let mut core = session.core(0)?;
    core.halt(std::time::Duration::from_millis(500))?;

    let pc: u32 = core.read_core_reg(core.program_counter())?;
    let sp: u32 = core.read_core_reg(core.stack_pointer())?;
    let lr: u32 = core.read_core_reg(core.return_address())?;

    let mut lines = vec![
        format!("Core registers (target: {}):", target),
        format!("  {:<8} = 0x{:08X}", "PC", pc),
        format!("  {:<8} = 0x{:08X}", "LR", lr),
        format!("  {:<8} = 0x{:08X}", "SP", sp),
    ];

    if is_cortex_m(target) {
        for (name, idx) in &[("R0", 0u16), ("R1", 1), ("R2", 2), ("R3", 3), ("R12", 12)] {
            if let Ok(val) = core.read_core_reg::<u32>(*idx) {
                lines.push(format!("  {:<8} = 0x{:08X}", name, val));
            }
        }
        if let Ok(val) = core.read_core_reg::<u32>(XPSR.id()) {
            lines.push(format!("  {:<8} = 0x{:08X}", "xPSR", val));
        }
        if let Ok(val) = core.read_core_reg::<u32>(0b10100u16) {
            lines.push(format!("  {:<8} = 0x{:08X}  (CONTROL[31:24] FAULTMASK[23:16] BASEPRI[15:8] PRIMASK[7:0])", "EXTRA", val));
        }
    } else {
        lines.push(format!("  (architecture: non-Cortex-M — PC/SP/LR only)"));
    }

    core.run()?;
    Ok(lines.join("\n"))
}

fn do_read_memory(probe_serial: Option<&str>, target: &str, address: u64, length: u32) -> Result<String> {
    let mut session = open_session(probe_serial, target)?;
    let mut core = session.core(0)?;
    let mut data = vec![0u8; length as usize];
    core.read_8(address, &mut data)?;
    let lines: Vec<String> = data.chunks(16).enumerate().map(|(i, chunk)| {
        let addr = address + (i * 16) as u64;
        let hex: Vec<String> = chunk.iter().map(|b| format!("{:02X}", b)).collect();
        let ascii: String = chunk.iter().map(|&b| if (0x20..0x7F).contains(&b) { b as char } else { '.' }).collect();
        format!("0x{:08X}  {:<48}  {}", addr, hex.join(" "), ascii)
    }).collect();
    Ok(lines.join("\n"))
}

fn do_write_memory(probe_serial: Option<&str>, target: &str, address: u64, data: &str) -> Result<String> {
    let clean: String = data.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    anyhow::ensure!(clean.len() % 2 == 0, "Hex data must have even number of nibbles");
    let bytes: Vec<u8> = (0..clean.len()).step_by(2)
        .map(|i| u8::from_str_radix(&clean[i..i + 2], 16))
        .collect::<std::result::Result<_, _>>()?;
    let mut session = open_session(probe_serial, target)?;
    let mut core = session.core(0)?;
    core.halt(std::time::Duration::from_millis(500))?;
    core.write_8(address, &bytes)?;
    core.run()?;
    Ok(format!("Wrote {} byte(s) to 0x{:08X}: {}",
        bytes.len(), address,
        bytes.iter().map(|b| format!("{:02X}", b)).collect::<Vec<_>>().join(" ")
    ))
}

fn do_read_call_stack(probe_serial: Option<&str>, target: &str) -> Result<String> {
    if !is_cortex_m(target) {
        return Ok(format!(
            "read_call_stack is Cortex-M specific (exception frame layout). \
             Target '{}' is not Cortex-M. For ESP32, decode the backtrace from the serial log instead.",
            target
        ));
    }
    let mut session = open_session(probe_serial, target)?;
    let mut core = session.core(0)?;
    core.halt(std::time::Duration::from_millis(500))?;

    let pc: u32 = core.read_core_reg(core.program_counter())?;
    let sp: u32 = core.read_core_reg(core.stack_pointer())?;
    let lr: u32 = core.read_core_reg(core.return_address())?;
    let xpsr: u32 = core.read_core_reg(XPSR.id())?;

    let mut frame_lines = Vec::new();
    for &(offset, name) in EXC_FRAME {
        let addr = (sp + offset) as u64;
        let mut buf = [0u32; 1];
        match core.read_32(addr, &mut buf) {
            Ok(_) => frame_lines.push(format!("  SP+0x{:02X}  {:<6} = 0x{:08X}", offset, name, buf[0])),
            Err(e) => frame_lines.push(format!("  SP+0x{:02X}  {:<6} = (read error: {})", offset, name, e)),
        }
    }
    core.run()?;

    Ok(format!(
        "Call stack (CM4):\n  {:<8} = 0x{:08X}  ← current instruction\n  {:<8} = 0x{:08X}  ← return address\n  {:<8} = 0x{:08X}\n  {:<8} = 0x{:08X}  (exception number: {})\n\nException frame at SP:\n{}",
        "PC", pc, "LR", lr, "SP", sp, "xPSR", xpsr, xpsr & 0x1FF,
        frame_lines.join("\n")
    ))
}

fn do_diagnose_hardfault(probe_serial: Option<&str>, target: &str) -> Result<String> {
    if !is_cortex_m(target) {
        return Ok(format!(
            "diagnose_hardfault is Cortex-M specific (CFSR/HFSR/BFAR registers). \
             Target '{}' is not Cortex-M. For ESP32 panics, read the backtrace from the serial log.",
            target
        ));
    }
    let mut session = open_session(probe_serial, target)?;
    let mut core = session.core(0)?;
    core.halt(std::time::Duration::from_millis(500))?;

    let mut buf = [0u32; 1];
    core.read_32(CFSR, &mut buf)?; let cfsr = buf[0];
    core.read_32(HFSR, &mut buf)?; let hfsr = buf[0];
    core.read_32(BFAR, &mut buf)?; let bfar = buf[0];
    core.read_32(MMFAR, &mut buf)?; let mmfar = buf[0];

    let pc: u32 = core.read_core_reg(core.program_counter())?;
    let sp: u32 = core.read_core_reg(core.stack_pointer())?;
    let fault_pc: Option<u32> = {
        let mut tmp = [0u32; 1];
        core.read_32((sp + 0x18) as u64, &mut tmp).ok().map(|_| tmp[0])
    };
    core.run()?;

    let mut lines = vec![
        "HardFault Diagnosis (CM4)".to_string(),
        format!("  HFSR  = 0x{:08X}", hfsr),
        format!("  CFSR  = 0x{:08X}", cfsr),
        format!("  PC    = 0x{:08X}", pc),
    ];
    if let Some(fpc) = fault_pc {
        lines.push(format!("  Fault PC (exception frame) = 0x{:08X}", fpc));
    }
    let hfsr_causes: Vec<&str> = HFSR_BITS.iter().filter(|(b, _)| hfsr & (1 << b) != 0).map(|(_, d)| *d).collect();
    if !hfsr_causes.is_empty() {
        lines.push("\nHFSR flags:".to_string());
        lines.extend(hfsr_causes.iter().map(|c| format!("  ⚠ {}", c)));
    }
    let cfsr_causes: Vec<&str> = CFSR_BITS.iter().filter(|(b, _)| cfsr & (1 << b) != 0).map(|(_, d)| *d).collect();
    if !cfsr_causes.is_empty() {
        lines.push("\nCFSR flags (root cause):".to_string());
        lines.extend(cfsr_causes.iter().map(|c| format!("  ⚠ {}", c)));
    }
    if cfsr & (1 << 15) != 0 { lines.push(format!("\n  BFAR  = 0x{:08X}  ← bus fault address", bfar)); }
    if cfsr & (1 << 7)  != 0 { lines.push(format!("  MMFAR = 0x{:08X}  ← MPU violation address", mmfar)); }
    if hfsr_causes.is_empty() && cfsr_causes.is_empty() {
        lines.push("\nNo fault bits set — CPU may not be in a fault state.".to_string());
    }
    Ok(lines.join("\n"))
}

fn do_set_breakpoint(probe_serial: Option<&str>, target: &str, address: u64) -> Result<String> {
    let mut session = open_session(probe_serial, target)?;
    let mut core = session.core(0)?;
    let available = core.available_breakpoint_units()?;
    core.set_hw_breakpoint(address)?;
    Ok(format!("Breakpoint set at 0x{:08X} ({} HW breakpoint unit(s) available)", address, available))
}

fn do_clear_breakpoint(probe_serial: Option<&str>, target: &str, address: u64) -> Result<String> {
    if !is_cortex_m(target) {
        return Ok(format!(
            "clear_breakpoint via FPB scan is Cortex-M specific. Target '{}' is not Cortex-M.",
            target
        ));
    }
    let mut session = open_session(probe_serial, target)?;
    let mut core = session.core(0)?;
    core.halt(std::time::Duration::from_millis(500))?;

    let mut buf = [0u32; 1];
    core.read_32(FP_CTRL, &mut buf)?;
    let num_code = ((buf[0] >> 4) & 0xF) as u64;
    let target_masked = (address as u32) & 0x1FFF_FFFC;
    let mut cleared_unit: Option<u64> = None;

    for i in 0..num_code {
        let comp_reg = FP_COMP_BASE + i * 4;
        core.read_32(comp_reg, &mut buf)?;
        if buf[0] & 1 != 0 && buf[0] & 0x1FFF_FFFC == target_masked {
            core.write_32(comp_reg, &[0u32])?;
            cleared_unit = Some(i);
            break;
        }
    }
    core.run()?;

    match cleared_unit {
        Some(i) => Ok(format!("Breakpoint cleared at 0x{:08X} (FPB unit {})", address, i)),
        None => Ok(format!("No breakpoint found at 0x{:08X}", address)),
    }
}

fn do_set_watchpoint(probe_serial: Option<&str>, target: &str, address: u64, kind: &str) -> Result<String> {
    if !is_cortex_m(target) {
        return Ok(format!(
            "set_watchpoint uses Cortex-M DWT registers. Target '{}' is not Cortex-M.",
            target
        ));
    }
    let func_val = match kind {
        "read" => DWT_FUNC_READ,
        "write" => DWT_FUNC_WRITE,
        "read_write" | "rw" => DWT_FUNC_READ_WRITE,
        _ => anyhow::bail!("Invalid kind '{}'. Use 'read', 'write', or 'read_write'", kind),
    };
    let mut session = open_session(probe_serial, target)?;
    let mut core = session.core(0)?;
    core.halt(std::time::Duration::from_millis(500))?;

    let mut buf = [0u32; 1];
    core.read_32(DEMCR, &mut buf)?;
    core.write_32(DEMCR, &[buf[0] | (1 << 24)])?;
    core.read_32(DWT_CTRL, &mut buf)?;
    let num_comp = ((buf[0] >> 28) & 0xF) as usize;
    let max = num_comp.min(DWT_MAX_COMPARATORS);

    let mut free_unit = None;
    for i in 0..max {
        core.read_32(dwt_func(i), &mut buf)?;
        if buf[0] & 0xF == DWT_FUNC_DISABLED { free_unit = Some(i); break; }
    }
    let unit = free_unit.with_context(|| format!("No free DWT comparators — all {} in use", max))?;

    core.write_32(dwt_comp(unit), &[address as u32])?;
    core.write_32(dwt_mask(unit), &[0u32])?;
    core.write_32(dwt_func(unit), &[func_val])?;
    core.run()?;

    Ok(format!("Watchpoint set: {} at 0x{:08X} (DWT unit {})", kind, address, unit))
}

fn do_clear_watchpoint(probe_serial: Option<&str>, target: &str, address: u64) -> Result<String> {
    if !is_cortex_m(target) {
        return Ok(format!(
            "clear_watchpoint uses Cortex-M DWT registers. Target '{}' is not Cortex-M.",
            target
        ));
    }
    let mut session = open_session(probe_serial, target)?;
    let mut core = session.core(0)?;
    core.halt(std::time::Duration::from_millis(500))?;

    let mut buf = [0u32; 1];
    core.read_32(DEMCR, &mut buf)?;
    core.write_32(DEMCR, &[buf[0] | (1 << 24)])?;

    let mut cleared = Vec::new();
    for i in 0..DWT_MAX_COMPARATORS {
        core.read_32(dwt_func(i), &mut buf)?;
        if buf[0] & 0xF == DWT_FUNC_DISABLED { continue; }
        core.read_32(dwt_comp(i), &mut buf)?;
        if buf[0] as u64 == address {
            core.write_32(dwt_func(i), &[DWT_FUNC_DISABLED])?;
            cleared.push(i);
        }
    }
    core.run()?;

    if cleared.is_empty() {
        Ok(format!("No active watchpoint at 0x{:08X}", address))
    } else {
        Ok(format!("Watchpoint cleared at 0x{:08X} (DWT unit(s): {})",
            address, cleared.iter().map(|u| u.to_string()).collect::<Vec<_>>().join(", ")))
    }
}

// ── MCP server ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct JtagMcp {
    tool_router: ToolRouter<Self>,
    registry: Arc<DeviceRegistry>,
    default_target: String,
}

impl JtagMcp {
    fn new(registry: DeviceRegistry, default_target: String) -> Self {
        Self {
            tool_router: Self::tool_router(),
            registry: Arc::new(registry),
            default_target,
        }
    }
}

// ── parameter structs ─────────────────────────────────────────────────────────

/// Common board selector added to every tool.
#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
struct BoardParam {
    /// Board alias from devices.toml (e.g. "board1"). If omitted, connects to first available probe.
    #[serde(default)]
    board: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ReadMemoryParams {
    #[serde(default)]
    board: Option<String>,
    /// Start address
    address: u64,
    /// Number of bytes (default: 64)
    #[serde(default = "default_length")]
    length: u32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct WriteMemoryParams {
    #[serde(default)]
    board: Option<String>,
    /// Start address
    address: u64,
    /// Bytes as hex string (e.g. "DEADBEEF" or "DE AD BE EF")
    data: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct AddressParam {
    #[serde(default)]
    board: Option<String>,
    /// Target address
    address: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct WatchpointParams {
    #[serde(default)]
    board: Option<String>,
    /// Address to watch
    address: u64,
    /// "read", "write" (default), or "read_write"
    #[serde(default = "default_watchpoint_kind")]
    kind: String,
}

fn default_length() -> u32 { 64 }
fn default_watchpoint_kind() -> String { "write".to_string() }

// ── tool implementations ──────────────────────────────────────────────────────

#[tool_router]
impl JtagMcp {
    #[tool(description = "List all connected debug probes with their serial numbers. Use serial numbers in devices.toml to pin each board to a specific probe.")]
    async fn list_probes(&self) -> String {
        do_list_probes().unwrap_or_else(|e| format!("ERROR: {}", e))
    }

    #[tool(description = "List all boards configured in devices.toml with their probe serial and target chip.")]
    async fn list_boards(&self) -> String {
        do_list_boards(&self.registry, &self.default_target)
    }

    #[tool(description = "Halt the CPU. Accepts optional board alias (e.g. 'board1'). Returns current PC.")]
    async fn halt_cpu(&self, Parameters(p): Parameters<BoardParam>) -> String {
        match resolve_board(p.board.as_deref(), &self.registry, &self.default_target) {
            Err(e) => format!("ERROR: {}", e),
            Ok((serial, target, _)) => do_halt_cpu(serial.as_deref(), &target).unwrap_or_else(|e| format!("ERROR: {}", e)),
        }
    }

    #[tool(description = "Resume the CPU after halt. Accepts optional board alias.")]
    async fn resume_cpu(&self, Parameters(p): Parameters<BoardParam>) -> String {
        match resolve_board(p.board.as_deref(), &self.registry, &self.default_target) {
            Err(e) => format!("ERROR: {}", e),
            Ok((serial, target, _)) => do_resume_cpu(serial.as_deref(), &target).unwrap_or_else(|e| format!("ERROR: {}", e)),
        }
    }

    #[tool(description = "Reset the target and resume. Returns PC observed after reset. Accepts optional board alias.")]
    async fn reset_target(&self, Parameters(p): Parameters<BoardParam>) -> String {
        match resolve_board(p.board.as_deref(), &self.registry, &self.default_target) {
            Err(e) => format!("ERROR: {}", e),
            Ok((serial, target, _)) => do_reset_target(serial.as_deref(), &target).unwrap_or_else(|e| format!("ERROR: {}", e)),
        }
    }

    #[tool(description = "Read CPU registers (PC, SP, LR; R0-R12/xPSR on Cortex-M). Halts then resumes. Accepts optional board alias.")]
    async fn read_registers(&self, Parameters(p): Parameters<BoardParam>) -> String {
        match resolve_board(p.board.as_deref(), &self.registry, &self.default_target) {
            Err(e) => format!("ERROR: {}", e),
            Ok((serial, target, _)) => do_read_registers(serial.as_deref(), &target).unwrap_or_else(|e| format!("ERROR: {}", e)),
        }
    }

    #[tool(description = "Read memory from the target and return a hex dump. Accepts optional board alias.")]
    async fn read_memory(&self, Parameters(p): Parameters<ReadMemoryParams>) -> String {
        match resolve_board(p.board.as_deref(), &self.registry, &self.default_target) {
            Err(e) => format!("ERROR: {}", e),
            Ok((serial, target, _)) => do_read_memory(serial.as_deref(), &target, p.address, p.length).unwrap_or_else(|e| format!("ERROR: {}", e)),
        }
    }

    #[tool(description = "Write bytes to target memory. Provide hex string (e.g. 'DEADBEEF'). Accepts optional board alias.")]
    async fn write_memory(&self, Parameters(p): Parameters<WriteMemoryParams>) -> String {
        match resolve_board(p.board.as_deref(), &self.registry, &self.default_target) {
            Err(e) => format!("ERROR: {}", e),
            Ok((serial, target, _)) => do_write_memory(serial.as_deref(), &target, p.address, &p.data).unwrap_or_else(|e| format!("ERROR: {}", e)),
        }
    }

    #[tool(description = "Read Cortex-M exception frame from stack (PC, LR, xPSR at fault). Not applicable to non-Cortex-M targets. Accepts optional board alias.")]
    async fn read_call_stack(&self, Parameters(p): Parameters<BoardParam>) -> String {
        match resolve_board(p.board.as_deref(), &self.registry, &self.default_target) {
            Err(e) => format!("ERROR: {}", e),
            Ok((serial, target, _)) => do_read_call_stack(serial.as_deref(), &target).unwrap_or_else(|e| format!("ERROR: {}", e)),
        }
    }

    #[tool(description = "Decode Cortex-M fault registers (HFSR/CFSR/BFAR/MMFAR). Not applicable to non-Cortex-M targets. Accepts optional board alias.")]
    async fn diagnose_hardfault(&self, Parameters(p): Parameters<BoardParam>) -> String {
        match resolve_board(p.board.as_deref(), &self.registry, &self.default_target) {
            Err(e) => format!("ERROR: {}", e),
            Ok((serial, target, _)) => do_diagnose_hardfault(serial.as_deref(), &target).unwrap_or_else(|e| format!("ERROR: {}", e)),
        }
    }

    #[tool(description = "Set a hardware breakpoint at the given address. CPU halts when PC reaches it. Accepts optional board alias.")]
    async fn set_breakpoint(&self, Parameters(p): Parameters<AddressParam>) -> String {
        match resolve_board(p.board.as_deref(), &self.registry, &self.default_target) {
            Err(e) => format!("ERROR: {}", e),
            Ok((serial, target, _)) => do_set_breakpoint(serial.as_deref(), &target, p.address).unwrap_or_else(|e| format!("ERROR: {}", e)),
        }
    }

    #[tool(description = "Clear a hardware breakpoint. Uses raw FPB scan (Cortex-M only). Accepts optional board alias.")]
    async fn clear_breakpoint(&self, Parameters(p): Parameters<AddressParam>) -> String {
        match resolve_board(p.board.as_deref(), &self.registry, &self.default_target) {
            Err(e) => format!("ERROR: {}", e),
            Ok((serial, target, _)) => do_clear_breakpoint(serial.as_deref(), &target, p.address).unwrap_or_else(|e| format!("ERROR: {}", e)),
        }
    }

    #[tool(description = "Set a DWT data watchpoint (Cortex-M only). kind = 'read', 'write' (default), or 'read_write'. Accepts optional board alias.")]
    async fn set_watchpoint(&self, Parameters(p): Parameters<WatchpointParams>) -> String {
        match resolve_board(p.board.as_deref(), &self.registry, &self.default_target) {
            Err(e) => format!("ERROR: {}", e),
            Ok((serial, target, _)) => do_set_watchpoint(serial.as_deref(), &target, p.address, &p.kind).unwrap_or_else(|e| format!("ERROR: {}", e)),
        }
    }

    #[tool(description = "Clear a DWT data watchpoint (Cortex-M only). Accepts optional board alias.")]
    async fn clear_watchpoint(&self, Parameters(p): Parameters<AddressParam>) -> String {
        match resolve_board(p.board.as_deref(), &self.registry, &self.default_target) {
            Err(e) => format!("ERROR: {}", e),
            Ok((serial, target, _)) => do_clear_watchpoint(serial.as_deref(), &target, p.address).unwrap_or_else(|e| format!("ERROR: {}", e)),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for JtagMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let registry = load_registry();
    let default_target = std::env::var("JTAG_MCP_TARGET")
        .unwrap_or_else(|_| "STM32WL55JCIx".to_string());
    let server = JtagMcp::new(registry, default_target);
    let transport = stdio();
    let handle = server.serve(transport).await?;
    handle.waiting().await?;
    Ok(())
}
