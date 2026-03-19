from fastmcp import FastMCP
from contextlib import contextmanager
from pyocd.core.helpers import ConnectHelper

mcp = FastMCP("jtag-mcp")

TARGET = "stm32wl55jcix"

# Cortex-M exception frame pushed to stack on fault (SP offsets)
_EXC_FRAME = {
    0x00: "R0", 0x04: "R1", 0x08: "R2", 0x0C: "R3",
    0x10: "R12", 0x14: "LR", 0x18: "PC", 0x1C: "xPSR",
}

# CFSR bit definitions (0xE000ED28)
_CFSR_BITS = {
    # MMFSR
    0: "IACCVIOL — Instruction access violation (MPU or execute-never fault)",
    1: "DACCVIOL — Data access violation (MPU fault)",
    3: "MUNSTKERR — MemManage fault on exception return unstacking",
    4: "MSTKERR — MemManage fault on exception entry stacking",
    5: "MLSPERR — MemManage fault during FP lazy state preservation",
    7: "MMARVALID — MMFAR holds valid fault address",
    # BFSR
    8:  "IBUSERR — Instruction bus error (prefetch abort)",
    9:  "PRECISERR — Precise data bus error (BFAR is valid)",
    10: "IMPRECISERR — Imprecise data bus error (async, DMA likely culprit)",
    11: "UNSTKERR — Bus fault on exception return unstacking",
    12: "STKERR — Bus fault on exception entry stacking",
    13: "LSPERR — Bus fault during FP lazy state preservation",
    15: "BFARVALID — BFAR holds valid fault address",
    # UFSR
    16: "UNDEFINSTR — Undefined instruction executed",
    17: "INVSTATE — Invalid EPSR state (Thumb bit not set)",
    18: "INVPC — Invalid EXC_RETURN on exception return",
    19: "NOCP — Coprocessor instruction with no coprocessor present",
    24: "UNALIGNED — Unaligned memory access (when UNALIGN_TRP is set)",
    25: "DIVBYZERO — Integer divide by zero",
}

# HFSR bit definitions (0xE000ED2C)
_HFSR_BITS = {
    1:  "VECTTBL — HardFault on vector table read (corrupted vector table?)",
    30: "FORCED — Escalated from configurable fault (check CFSR for root cause)",
    31: "DEBUGEVT — Debug event (breakpoint/watchpoint triggered HardFault)",
}


@contextmanager
def _session(halt: bool = False):
    with ConnectHelper.session_with_chosen_probe(
        target_override=TARGET,
        options={"frequency": 1000000, "connect_mode": "halt" if halt else "attach"},
    ) as session:
        yield session.board.target


@mcp.tool()
def halt_cpu() -> str:
    """Halt the CM4 core. Always call this before reading registers or memory."""
    with _session() as target:
        target.halt()
        pc = target.read_core_register("pc")
        return f"CPU halted. PC = 0x{pc:08X}"


@mcp.tool()
def resume_cpu() -> str:
    """Resume the CM4 core after halt."""
    with _session() as target:
        target.resume()
        return "CPU resumed."


@mcp.tool()
def read_registers() -> str:
    """
    Read key CM4 core registers (PC, LR, SP, PSR, CONTROL).
    Halts the CPU, reads, then resumes. Safe to call at any time.
    """
    with _session() as target:
        target.halt()
        regs = {
            r: target.read_core_register(r)
            for r in ["pc", "lr", "sp", "xpsr", "control", "r0", "r1", "r2", "r3", "r12"]
        }
        target.resume()

    lines = [f"  {k.upper():<8} = 0x{v:08X}" for k, v in regs.items()]
    return "Core registers (CM4):\n" + "\n".join(lines)


@mcp.tool()
def read_memory(address: int, length: int = 64) -> str:
    """
    Read memory from the target and return a hex dump.
    CPU must be halted first (call halt_cpu).

    Args:
        address: Start address in hex (e.g. 0x20000000 for SRAM).
        length: Number of bytes to read. Default: 64.
    """
    with _session() as target:
        data = target.read_memory_block8(address, length)

    lines = []
    for i in range(0, len(data), 16):
        chunk = data[i:i + 16]
        hex_part = " ".join(f"{b:02X}" for b in chunk)
        ascii_part = "".join(chr(b) if 32 <= b < 127 else "." for b in chunk)
        lines.append(f"0x{address + i:08X}  {hex_part:<48}  {ascii_part}")
    return "\n".join(lines)


@mcp.tool()
def read_call_stack() -> str:
    """
    Halt the CPU and read the exception frame from the stack to reconstruct
    the call context at the point of fault. Returns PC, LR, SP and the
    exception frame registers. Best called immediately after a HardFault.
    """
    with _session() as target:
        target.halt()
        sp = target.read_core_register("sp")
        pc = target.read_core_register("pc")
        lr = target.read_core_register("lr")
        xpsr = target.read_core_register("xpsr")

        # Read exception frame from stack
        frame_lines = []
        try:
            for offset, name in _EXC_FRAME.items():
                val = target.read32(sp + offset)
                frame_lines.append(f"  SP+0x{offset:02X}  {name:<6} = 0x{val:08X}")
        except Exception as e:
            frame_lines.append(f"  (could not read exception frame: {e})")

        target.resume()

    return (
        f"Call stack (CM4):\n"
        f"  PC       = 0x{pc:08X}  ← current instruction\n"
        f"  LR       = 0x{lr:08X}  ← return address\n"
        f"  SP       = 0x{sp:08X}\n"
        f"  xPSR     = 0x{xpsr:08X}  (exception number: {xpsr & 0x1FF})\n"
        f"\nException frame at SP:\n" + "\n".join(frame_lines)
    )


@mcp.tool()
def diagnose_hardfault() -> str:
    """
    Read and decode the Cortex-M fault status registers (HFSR + CFSR + BFAR + MMFAR).
    Translates raw register values into human-readable root cause descriptions.
    Call this immediately after a HardFault is detected in the serial log.
    """
    CFSR  = 0xE000ED28
    HFSR  = 0xE000ED2C
    BFAR  = 0xE000ED38
    MMFAR = 0xE000ED34

    with _session() as target:
        target.halt()
        cfsr  = target.read32(CFSR)
        hfsr  = target.read32(HFSR)
        bfar  = target.read32(BFAR)
        mmfar = target.read32(MMFAR)
        sp    = target.read_core_register("sp")
        pc    = target.read_core_register("pc")

        # Try to read faulting PC from exception frame
        try:
            fault_pc = target.read32(sp + 0x18)
        except Exception:
            fault_pc = None

        target.resume()

    lines = [
        f"HardFault Diagnosis (CM4)",
        f"  HFSR  = 0x{hfsr:08X}",
        f"  CFSR  = 0x{cfsr:08X}",
        f"  PC    = 0x{pc:08X}",
    ]
    if fault_pc:
        lines.append(f"  Fault PC (from exception frame) = 0x{fault_pc:08X}")

    # Decode HFSR
    hfsr_causes = [desc for bit, desc in _HFSR_BITS.items() if hfsr & (1 << bit)]
    if hfsr_causes:
        lines.append("\nHFSR flags:")
        lines.extend(f"  ⚠ {c}" for c in hfsr_causes)

    # Decode CFSR
    cfsr_causes = [desc for bit, desc in _CFSR_BITS.items() if cfsr & (1 << bit)]
    if cfsr_causes:
        lines.append("\nCFSR flags (root cause):")
        lines.extend(f"  ⚠ {c}" for c in cfsr_causes)

    # Append valid fault addresses
    if cfsr & (1 << 15):  # BFARVALID
        lines.append(f"\n  BFAR  = 0x{bfar:08X}  ← bus fault at this address")
    if cfsr & (1 << 7):   # MMARVALID
        lines.append(f"  MMFAR = 0x{mmfar:08X}  ← memory access violation at this address")

    if not hfsr_causes and not cfsr_causes:
        lines.append("\nNo fault bits set — CPU may not be in a fault state.")

    return "\n".join(lines)


if __name__ == "__main__":
    mcp.run()
