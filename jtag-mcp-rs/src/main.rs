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

const TARGET: &str = "STM32WL55JCIx";

// Cortex-M fault status register addresses
const CFSR: u64 = 0xE000_ED28;
const HFSR: u64 = 0xE000_ED2C;
const BFAR: u64 = 0xE000_ED38;
const MMFAR: u64 = 0xE000_ED34;

// CoreDebug / DWT registers (Cortex-M4)
const DEMCR: u64 = 0xE000_EDFC; // Debug Exception and Monitor Control
const FP_CTRL: u64 = 0xE000_2000; // FPB Control (bits [7:4] = NUM_CODE)
const FP_COMP_BASE: u64 = 0xE000_2008; // FP_COMP0; each unit is 4 bytes apart
const DWT_CTRL: u64 = 0xE000_1000; // DWT Control (bits [31:28] = NUMCOMP)
const DWT_COMP_BASE: u64 = 0xE000_1020; // COMP0; each unit is 0x10 bytes apart

// STM32WL55 CM4 has 4 DWT comparator units
const DWT_MAX_COMPARATORS: usize = 4;

fn dwt_comp(unit: usize) -> u64 { DWT_COMP_BASE + (unit as u64) * 0x10 }
fn dwt_mask(unit: usize) -> u64 { DWT_COMP_BASE + (unit as u64) * 0x10 + 0x4 }
fn dwt_func(unit: usize) -> u64 { DWT_COMP_BASE + (unit as u64) * 0x10 + 0x8 }

// DWT FUNCTION[3:0] values for halt-mode data watchpoints
const DWT_FUNC_DISABLED: u32 = 0b0000;
const DWT_FUNC_READ: u32 = 0b0101;
const DWT_FUNC_WRITE: u32 = 0b0110;
const DWT_FUNC_READ_WRITE: u32 = 0b0111;

// Exception frame offsets from SP
const EXC_FRAME: &[(u32, &str)] = &[
    (0x00, "R0"),
    (0x04, "R1"),
    (0x08, "R2"),
    (0x0C, "R3"),
    (0x10, "R12"),
    (0x14, "LR"),
    (0x18, "PC"),
    (0x1C, "xPSR"),
];

const CFSR_BITS: &[(u32, &str)] = &[
    (0,  "IACCVIOL — Instruction access violation (MPU or execute-never fault)"),
    (1,  "DACCVIOL — Data access violation (MPU fault)"),
    (3,  "MUNSTKERR — MemManage fault on exception return unstacking"),
    (4,  "MSTKERR — MemManage fault on exception entry stacking"),
    (5,  "MLSPERR — MemManage fault during FP lazy state preservation"),
    (7,  "MMARVALID — MMFAR holds valid fault address"),
    (8,  "IBUSERR — Instruction bus error (prefetch abort)"),
    (9,  "PRECISERR — Precise data bus error (BFAR is valid)"),
    (10, "IMPRECISERR — Imprecise data bus error (async, DMA likely culprit)"),
    (11, "UNSTKERR — Bus fault on exception return unstacking"),
    (12, "STKERR — Bus fault on exception entry stacking"),
    (13, "LSPERR — Bus fault during FP lazy state preservation"),
    (15, "BFARVALID — BFAR holds valid fault address"),
    (16, "UNDEFINSTR — Undefined instruction executed"),
    (17, "INVSTATE — Invalid EPSR state (Thumb bit not set)"),
    (18, "INVPC — Invalid EXC_RETURN on exception return"),
    (19, "NOCP — Coprocessor instruction with no coprocessor present"),
    (24, "UNALIGNED — Unaligned memory access (when UNALIGN_TRP is set)"),
    (25, "DIVBYZERO — Integer divide by zero"),
];

const HFSR_BITS: &[(u32, &str)] = &[
    (1,  "VECTTBL — HardFault on vector table read (corrupted vector table?)"),
    (30, "FORCED — Escalated from configurable fault (check CFSR for root cause)"),
    (31, "DEBUGEVT — Debug event (breakpoint/watchpoint triggered HardFault)"),
];

fn open_session() -> Result<Session> {
    let lister = Lister::new();
    let probes = lister.list_all();
    let probe_info = probes
        .into_iter()
        .next()
        .context("No debug probe found. Is the J-Link/ST-Link connected?")?;
    let probe = probe_info.open().context("Failed to open debug probe")?;
    probe
        .attach(TARGET, Permissions::default())
        .context("Failed to attach to target")
}

// ── tool implementations ─────────────────────────────────────────────────────

fn do_halt_cpu() -> Result<String> {
    let mut session = open_session()?;
    let mut core = session.core(0)?;
    core.halt(std::time::Duration::from_millis(500))?;
    let pc: u32 = core.read_core_reg(core.program_counter())?;
    Ok(format!("CPU halted. PC = 0x{:08X}", pc))
}

fn do_resume_cpu() -> Result<String> {
    let mut session = open_session()?;
    let mut core = session.core(0)?;
    core.run()?;
    Ok("CPU resumed.".to_string())
}

fn do_read_registers() -> Result<String> {
    let mut session = open_session()?;
    let mut core = session.core(0)?;
    core.halt(std::time::Duration::from_millis(500))?;

    let pc: u32 = core.read_core_reg(core.program_counter())?;
    let sp: u32 = core.read_core_reg(core.stack_pointer())?;
    let lr: u32 = core.read_core_reg(core.return_address())?;

    let mut lines = vec![
        "Core registers (CM4):".to_string(),
        format!("  {:<8} = 0x{:08X}", "PC", pc),
        format!("  {:<8} = 0x{:08X}", "LR", lr),
        format!("  {:<8} = 0x{:08X}", "SP", sp),
    ];

    // R0–R3, R12 by RegisterId (u16)
    for (name, idx) in &[("R0", 0u16), ("R1", 1), ("R2", 2), ("R3", 3), ("R12", 12)] {
        if let Ok(val) = core.read_core_reg::<u32>(*idx) {
            lines.push(format!("  {:<8} = 0x{:08X}", name, val));
        }
    }

    // xPSR and CONTROL
    if let Ok(val) = core.read_core_reg::<u32>(XPSR.id()) {
        lines.push(format!("  {:<8} = 0x{:08X}", "xPSR", val));
    }
    // probe-rs packs CONTROL[31:24] | FAULTMASK[23:16] | BASEPRI[15:8] | PRIMASK[7:0] at id 0b10100
    if let Ok(val) = core.read_core_reg::<u32>(0b10100u16) {
        lines.push(format!("  {:<8} = 0x{:08X}  (CONTROL[31:24] FAULTMASK[23:16] BASEPRI[15:8] PRIMASK[7:0])", "EXTRA", val));
    }

    core.run()?;
    Ok(lines.join("\n"))
}

fn do_read_memory(address: u64, length: u32) -> Result<String> {
    let mut session = open_session()?;
    let mut core = session.core(0)?;

    let mut data = vec![0u8; length as usize];
    core.read_8(address, &mut data)?;

    let lines: Vec<String> = data
        .chunks(16)
        .enumerate()
        .map(|(i, chunk)| {
            let addr = address + (i * 16) as u64;
            let hex: Vec<String> = chunk.iter().map(|b| format!("{:02X}", b)).collect();
            let ascii: String = chunk
                .iter()
                .map(|&b| if (0x20..0x7F).contains(&b) { b as char } else { '.' })
                .collect();
            format!("0x{:08X}  {:<48}  {}", addr, hex.join(" "), ascii)
        })
        .collect();

    Ok(lines.join("\n"))
}

fn do_write_memory(address: u64, data: &str) -> Result<String> {
    // Accept hex strings with or without spaces/0x prefixes, e.g. "DEADBEEF" or "DE AD BE EF"
    let clean: String = data.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    anyhow::ensure!(
        clean.len() % 2 == 0,
        "Hex data must have an even number of nibbles, got {} ('{}')",
        clean.len(),
        data
    );
    let bytes: Vec<u8> = (0..clean.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&clean[i..i + 2], 16))
        .collect::<std::result::Result<_, _>>()?;

    let mut session = open_session()?;
    let mut core = session.core(0)?;
    core.halt(std::time::Duration::from_millis(500))?;
    core.write_8(address, &bytes)?;
    core.run()?;

    Ok(format!(
        "Wrote {} byte(s) to 0x{:08X}: {}",
        bytes.len(),
        address,
        bytes.iter().map(|b| format!("{:02X}", b)).collect::<Vec<_>>().join(" ")
    ))
}

fn do_reset_target() -> Result<String> {
    let mut session = open_session()?;
    let mut core = session.core(0)?;
    core.reset_and_halt(std::time::Duration::from_millis(500))?;
    let pc: u32 = core.read_core_reg(core.program_counter())?;
    core.run()?;
    Ok(format!(
        "Target reset. PC after reset = 0x{:08X}. CPU resumed.",
        pc
    ))
}

fn do_read_call_stack() -> Result<String> {
    let mut session = open_session()?;
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

fn do_diagnose_hardfault() -> Result<String> {
    let mut session = open_session()?;
    let mut core = session.core(0)?;
    core.halt(std::time::Duration::from_millis(500))?;

    let mut buf = [0u32; 1];
    core.read_32(CFSR, &mut buf)?;
    let cfsr = buf[0];
    core.read_32(HFSR, &mut buf)?;
    let hfsr = buf[0];
    core.read_32(BFAR, &mut buf)?;
    let bfar = buf[0];
    core.read_32(MMFAR, &mut buf)?;
    let mmfar = buf[0];

    let pc: u32 = core.read_core_reg(core.program_counter())?;
    let sp: u32 = core.read_core_reg(core.stack_pointer())?;

    // Faulting PC from exception frame at SP+0x18
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
        lines.push(format!("  Fault PC (from exception frame) = 0x{:08X}", fpc));
    }

    let hfsr_causes: Vec<&str> = HFSR_BITS
        .iter()
        .filter(|(bit, _)| hfsr & (1 << bit) != 0)
        .map(|(_, desc)| *desc)
        .collect();

    if !hfsr_causes.is_empty() {
        lines.push("\nHFSR flags:".to_string());
        lines.extend(hfsr_causes.iter().map(|c| format!("  ⚠ {}", c)));
    }

    let cfsr_causes: Vec<&str> = CFSR_BITS
        .iter()
        .filter(|(bit, _)| cfsr & (1 << bit) != 0)
        .map(|(_, desc)| *desc)
        .collect();

    if !cfsr_causes.is_empty() {
        lines.push("\nCFSR flags (root cause):".to_string());
        lines.extend(cfsr_causes.iter().map(|c| format!("  ⚠ {}", c)));
    }

    if cfsr & (1 << 15) != 0 {
        lines.push(format!("\n  BFAR  = 0x{:08X}  ← bus fault at this address", bfar));
    }
    if cfsr & (1 << 7) != 0 {
        lines.push(format!("  MMFAR = 0x{:08X}  ← memory access violation at this address", mmfar));
    }

    if hfsr_causes.is_empty() && cfsr_causes.is_empty() {
        lines.push("\nNo fault bits set — CPU may not be in a fault state.".to_string());
    }

    Ok(lines.join("\n"))
}

fn do_set_breakpoint(address: u64) -> Result<String> {
    let mut session = open_session()?;
    let mut core = session.core(0)?;
    let available = core.available_breakpoint_units()?;
    core.set_hw_breakpoint(address)?;
    Ok(format!(
        "Breakpoint set at 0x{:08X} ({} HW breakpoint unit(s) available on this target)",
        address, available
    ))
}

fn do_clear_breakpoint(address: u64) -> Result<String> {
    let mut session = open_session()?;
    let mut core = session.core(0)?;
    core.halt(std::time::Duration::from_millis(500))?;

    // probe-rs clear_hw_breakpoint fails when called from a new session that didn't
    // set the breakpoint (no internal state). Use raw FPB register access instead.
    //
    // FP_CTRL[7:4] = NUM_CODE (number of code comparators)
    // FP_COMPn[28:2] = COMP address bits, FP_COMPn[0] = ENABLE
    // To clear: find the comparator matching address, write 0 to disable it.
    let mut buf = [0u32; 1];
    core.read_32(FP_CTRL, &mut buf)?;
    let num_code = ((buf[0] >> 4) & 0xF) as u64;

    let target_masked = (address as u32) & 0x1FFF_FFFC; // address bits [28:2] in place
    let mut cleared_unit: Option<u64> = None;

    for i in 0..num_code {
        let comp_reg = FP_COMP_BASE + i * 4;
        core.read_32(comp_reg, &mut buf)?;
        let enabled = buf[0] & 1;
        let comp_masked = buf[0] & 0x1FFF_FFFC;
        if enabled != 0 && comp_masked == target_masked {
            core.write_32(comp_reg, &[0u32])?;
            cleared_unit = Some(i);
            break;
        }
    }

    core.run()?;

    match cleared_unit {
        Some(i) => Ok(format!(
            "Breakpoint cleared at 0x{:08X} (FPB comparator unit {})",
            address, i
        )),
        None => Ok(format!(
            "No breakpoint found at 0x{:08X} (may already be cleared)",
            address
        )),
    }
}

fn do_set_watchpoint(address: u64, kind: &str) -> Result<String> {
    let func_val = match kind {
        "read" => DWT_FUNC_READ,
        "write" => DWT_FUNC_WRITE,
        "read_write" | "rw" => DWT_FUNC_READ_WRITE,
        _ => anyhow::bail!(
            "Invalid watchpoint kind '{}'. Use 'read', 'write', or 'read_write'",
            kind
        ),
    };

    let mut session = open_session()?;
    let mut core = session.core(0)?;
    core.halt(std::time::Duration::from_millis(500))?;

    // Enable DWT via DEMCR.TRCENA (bit 24) — required before accessing DWT registers
    let mut buf = [0u32; 1];
    core.read_32(DEMCR, &mut buf)?;
    core.write_32(DEMCR, &[buf[0] | (1 << 24)])?;

    // Read NUMCOMP from DWT_CTRL bits [31:28]
    core.read_32(DWT_CTRL, &mut buf)?;
    let num_comp = ((buf[0] >> 28) & 0xF) as usize;
    let max = num_comp.min(DWT_MAX_COMPARATORS);

    // Find a free comparator (FUNCTION[3:0] == 0)
    let mut free_unit = None;
    for i in 0..max {
        core.read_32(dwt_func(i), &mut buf)?;
        if buf[0] & 0xF == DWT_FUNC_DISABLED {
            free_unit = Some(i);
            break;
        }
    }

    let unit = free_unit.with_context(|| {
        format!("No free DWT comparator units — all {} in use", max)
    })?;

    core.write_32(dwt_comp(unit), &[address as u32])?; // comparator address
    core.write_32(dwt_mask(unit), &[0u32])?;            // exact address match (mask = 0)
    core.write_32(dwt_func(unit), &[func_val])?;        // enable watchpoint

    core.run()?;

    Ok(format!(
        "Watchpoint set: {} at 0x{:08X} (DWT comparator unit {})",
        kind, address, unit
    ))
}

fn do_clear_watchpoint(address: u64) -> Result<String> {
    let mut session = open_session()?;
    let mut core = session.core(0)?;
    core.halt(std::time::Duration::from_millis(500))?;

    // Enable TRCENA so DWT registers are accessible
    let mut buf = [0u32; 1];
    core.read_32(DEMCR, &mut buf)?;
    core.write_32(DEMCR, &[buf[0] | (1 << 24)])?;

    let mut cleared_units = Vec::new();
    for i in 0..DWT_MAX_COMPARATORS {
        core.read_32(dwt_func(i), &mut buf)?;
        if buf[0] & 0xF == DWT_FUNC_DISABLED {
            continue; // already disabled, skip
        }
        core.read_32(dwt_comp(i), &mut buf)?;
        if buf[0] as u64 == address {
            core.write_32(dwt_func(i), &[DWT_FUNC_DISABLED])?;
            cleared_units.push(i);
        }
    }

    core.run()?;

    if cleared_units.is_empty() {
        Ok(format!("No active watchpoint found at 0x{:08X}", address))
    } else {
        Ok(format!(
            "Watchpoint cleared at 0x{:08X} (unit(s): {})",
            address,
            cleared_units.iter().map(|u| u.to_string()).collect::<Vec<_>>().join(", ")
        ))
    }
}

// ── MCP server ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct JtagMcp {
    tool_router: ToolRouter<Self>,
}

impl JtagMcp {
    fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ReadMemoryParams {
    /// Start address (e.g. 536870912 for 0x20000000 SRAM)
    address: u64,
    /// Number of bytes to read (default: 64)
    #[serde(default = "default_length")]
    length: u32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct WriteMemoryParams {
    /// Start address (e.g. 536870912 for 0x20000000 SRAM)
    address: u64,
    /// Bytes to write as a hex string, with or without spaces (e.g. "DEADBEEF" or "DE AD BE EF")
    data: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct AddressParam {
    /// Target address (e.g. 134225666 for 0x08004102)
    address: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct WatchpointParams {
    /// Address to watch (e.g. 536870912 for 0x20000000)
    address: u64,
    /// Watchpoint kind: "read", "write", or "read_write" (default: "write")
    #[serde(default = "default_watchpoint_kind")]
    kind: String,
}

fn default_length() -> u32 {
    64
}

fn default_watchpoint_kind() -> String {
    "write".to_string()
}

#[tool_router]
impl JtagMcp {
    #[tool(description = "Halt the CM4 core. Always call this before reading registers or memory.")]
    async fn halt_cpu(&self) -> String {
        do_halt_cpu().unwrap_or_else(|e| format!("ERROR: {}", e))
    }

    #[tool(description = "Resume the CM4 core after halt.")]
    async fn resume_cpu(&self) -> String {
        do_resume_cpu().unwrap_or_else(|e| format!("ERROR: {}", e))
    }

    #[tool(description = "Reset the target and resume. Returns the PC observed immediately after reset.")]
    async fn reset_target(&self) -> String {
        do_reset_target().unwrap_or_else(|e| format!("ERROR: {}", e))
    }

    #[tool(description = "Read key CM4 core registers (PC, LR, SP, R0-R3, R12). Halts then resumes.")]
    async fn read_registers(&self) -> String {
        do_read_registers().unwrap_or_else(|e| format!("ERROR: {}", e))
    }

    #[tool(description = "Read memory from the target and return a hex dump. Provide address and optional length (default 64 bytes).")]
    async fn read_memory(&self, Parameters(p): Parameters<ReadMemoryParams>) -> String {
        do_read_memory(p.address, p.length).unwrap_or_else(|e| format!("ERROR: {}", e))
    }

    #[tool(description = "Write bytes to target memory. Provide address and data as a hex string (e.g. 'DEADBEEF'). Halts CPU during write.")]
    async fn write_memory(&self, Parameters(p): Parameters<WriteMemoryParams>) -> String {
        do_write_memory(p.address, &p.data).unwrap_or_else(|e| format!("ERROR: {}", e))
    }

    #[tool(description = "Halt CPU and read the exception frame from the stack. Best called immediately after a HardFault.")]
    async fn read_call_stack(&self) -> String {
        do_read_call_stack().unwrap_or_else(|e| format!("ERROR: {}", e))
    }

    #[tool(description = "Read and decode Cortex-M fault status registers (HFSR + CFSR + BFAR + MMFAR). Call immediately after a HardFault.")]
    async fn diagnose_hardfault(&self) -> String {
        do_diagnose_hardfault().unwrap_or_else(|e| format!("ERROR: {}", e))
    }

    #[tool(description = "Set a hardware breakpoint at the given address (uses FPB unit). CPU halts when PC reaches this address.")]
    async fn set_breakpoint(&self, Parameters(p): Parameters<AddressParam>) -> String {
        do_set_breakpoint(p.address).unwrap_or_else(|e| format!("ERROR: {}", e))
    }

    #[tool(description = "Clear a hardware breakpoint previously set at the given address.")]
    async fn clear_breakpoint(&self, Parameters(p): Parameters<AddressParam>) -> String {
        do_clear_breakpoint(p.address).unwrap_or_else(|e| format!("ERROR: {}", e))
    }

    #[tool(description = "Set a DWT data watchpoint. CPU halts when the watched address is accessed. kind = 'read', 'write' (default), or 'read_write'.")]
    async fn set_watchpoint(&self, Parameters(p): Parameters<WatchpointParams>) -> String {
        do_set_watchpoint(p.address, &p.kind).unwrap_or_else(|e| format!("ERROR: {}", e))
    }

    #[tool(description = "Clear a DWT data watchpoint at the given address.")]
    async fn clear_watchpoint(&self, Parameters(p): Parameters<AddressParam>) -> String {
        do_clear_watchpoint(p.address).unwrap_or_else(|e| format!("ERROR: {}", e))
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
    let server = JtagMcp::new();
    let transport = stdio();
    let handle = server.serve(transport).await?;
    handle.waiting().await?;
    Ok(())
}
