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

fn default_length() -> u32 {
    64
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

    #[tool(description = "Read key CM4 core registers (PC, LR, SP, R0-R3, R12). Halts then resumes.")]
    async fn read_registers(&self) -> String {
        do_read_registers().unwrap_or_else(|e| format!("ERROR: {}", e))
    }

    #[tool(description = "Read memory from the target and return a hex dump. Provide address and optional length (default 64 bytes).")]
    async fn read_memory(&self, Parameters(p): Parameters<ReadMemoryParams>) -> String {
        do_read_memory(p.address, p.length).unwrap_or_else(|e| format!("ERROR: {}", e))
    }

    #[tool(description = "Halt CPU and read the exception frame from the stack. Best called immediately after a HardFault.")]
    async fn read_call_stack(&self) -> String {
        do_read_call_stack().unwrap_or_else(|e| format!("ERROR: {}", e))
    }

    #[tool(description = "Read and decode Cortex-M fault status registers (HFSR + CFSR + BFAR + MMFAR). Call immediately after a HardFault.")]
    async fn diagnose_hardfault(&self) -> String {
        do_diagnose_hardfault().unwrap_or_else(|e| format!("ERROR: {}", e))
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
