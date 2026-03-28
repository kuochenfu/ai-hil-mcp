# jtag-mcp User Manual

**Version:** 2.0 · 2026-03-29

> Multi-board JTAG/SWD debug layer for AI-HIL — probe selection by serial number, per-board target configuration, CPU halt/resume/reset, register and memory inspection, and Cortex-M fault diagnosis.

---

## Overview

`jtag-mcp` gives Claude Code the ability to connect to and debug physical embedded hardware via JTAG or SWD. Version 2.0 adds a **device registry** (`devices.toml`) that maps board names to probe serial numbers and target chip identifiers, plus architecture-aware guards that return clear guidance when Cortex-M-specific tools are called on non-Cortex-M targets (e.g. ESP32).

### Tool Summary

| Tool | Category | Description |
|------|----------|-------------|
| `list_probes` | Discovery | Enumerate all connected debug probes with serial numbers |
| `list_boards` | Discovery | Show boards configured in `devices.toml` with probe and target info |
| `halt_cpu` | Control | Halt the CPU and return current PC |
| `resume_cpu` | Control | Resume execution after halt |
| `reset_target` | Control | Soft reset; returns PC observed after reset |
| `read_registers` | Perception | PC, SP, LR; full Cortex-M register set on CM targets |
| `read_memory` | Perception | Hex dump of any memory address range |
| `write_memory` | Action | Write bytes to any address (fault injection, live patches) |
| `read_call_stack` | Perception | Cortex-M exception frame at SP — PC, LR, xPSR at fault |
| `diagnose_hardfault` | Perception | Decode HFSR/CFSR/BFAR/MMFAR — semantic fault report (Cortex-M only) |
| `set_breakpoint` | Action | Hardware breakpoint via FPB — CPU halts when PC reaches address |
| `clear_breakpoint` | Action | Remove FPB breakpoint by raw register scan |
| `set_watchpoint` | Action | DWT data watchpoint — halt on read/write/read_write (Cortex-M only) |
| `clear_watchpoint` | Action | Remove DWT watchpoint |

---

## Board Configuration

### Option A — Manual configuration in `devices.toml` (recommended)

The device registry is the same file used by `serial-mcp`. It lives at the path pointed to by the `JTAG_MCP_CONFIG` environment variable (already set in `.mcp.json`). Default location: `/Users/chenfu/Labs/ai-hil-mcp/devices.toml`.

**Step 1 — discover connected probes:**

```
list_probes()
```

This returns every connected debug probe with its VID/PID and hardware serial number. The serial number is stable across reboots and reconnects — use it to pin each board alias to a specific physical probe.

Example output:
```
[0] ESP JTAG — VID:303A PID:1001 — serial: 90:E5:B1:CB:CE:BC
[1] ESP JTAG — VID:303A PID:1001 — serial: 90:E5:B1:CB:CD:84
```

**Step 2 — edit `devices.toml`:**

```toml
[board.board1]
description  = "Espressif board 1"
probe_serial = "90:E5:B1:CB:CE:BC"   # from list_probes
target       = "esp32s3"              # probe-rs target chip name

[board.board2]
description  = "Espressif board 2"
probe_serial = "90:E5:B1:CB:CD:84"
target       = "esp32s3"
```

The same `devices.toml` also holds serial port config for `serial-mcp`. Both servers share the file and each reads only the fields it needs.

**JTAG fields:**
| Field | Required | Default | Description |
|-------|----------|---------|-------------|
| `probe_serial` | no | first available | Hardware serial number from `list_probes` |
| `target` | no | `JTAG_MCP_TARGET` env var or `STM32WL55JCIx` | probe-rs target chip identifier |
| `description` | no | — | Free-text label shown in `list_boards` |

Changes to `devices.toml` take effect immediately — no rebuild or restart needed.

**Common target names:**

| Chip | probe-rs target string |
|------|----------------------|
| ESP32-S3 | `esp32s3` |
| ESP32-C3 | `esp32c3` |
| ESP32 | `esp32` |
| STM32WL55JC | `STM32WL55JCIx` |
| STM32F4 series | `STM32F407VGTx` (varies by part) |
| nRF52840 | `nRF52840_xxAA` |

Run `probe-rs chip list` in a terminal to search for any chip.

### Option B — No config (single-board, default target)

When no `board` parameter is passed to any tool, jtag-mcp connects to the first available probe and uses:
1. `JTAG_MCP_TARGET` environment variable, or
2. `STM32WL55JCIx` as the hardcoded fallback

This preserves backward compatibility with single-board STM32 workflows.

### Per-project configuration

The binary is shared across all projects — only the config path changes. Each project sets `JTAG_MCP_CONFIG` in its own `.mcp.json`:

```json
"jtag-mcp": {
  "type": "stdio",
  "command": "/Users/chenfu/Labs/ai-hil-mcp/jtag-mcp-rs/target/release/jtag-mcp-rs",
  "args": [],
  "env": {
    "JTAG_MCP_CONFIG": "/Users/chenfu/Labs/<your-project>/devices.toml"
  }
}
```

When you open Claude Code in a different project directory, it reads that project's `.mcp.json` and loads the right `devices.toml` automatically. No rebuild needed — just update the TOML.

---

## Architecture Awareness

Some tools are **Cortex-M specific** — they read ARM debug registers that do not exist on other architectures (ESP32 Xtensa/RISC-V, RISC-V MCUs, etc.). When called on a non-Cortex-M target, these tools return a clear guidance message instead of an error.

| Tool | Cortex-M only? | Fallback for non-CM |
|------|---------------|---------------------|
| `halt_cpu` | No | Works on all targets |
| `resume_cpu` | No | Works (note: ESP32 has probe-rs quirk — use `reset_target` instead) |
| `reset_target` | No | Works on all targets |
| `read_registers` | Partial | PC/SP/LR on all targets; R0–R12/xPSR/CONTROL only on Cortex-M |
| `read_memory` | No | Works on all targets |
| `write_memory` | No | Works on all targets |
| `set_breakpoint` | No | Works via probe-rs abstraction |
| `read_call_stack` | **Yes** | Returns: "use serial log backtrace for ESP32" |
| `diagnose_hardfault` | **Yes** | Returns: "use serial log backtrace for ESP32" |
| `clear_breakpoint` | **Yes** (FPB scan) | Returns architecture note |
| `set_watchpoint` | **Yes** (DWT) | Returns architecture note |
| `clear_watchpoint` | **Yes** (DWT) | Returns architecture note |

**ESP32 note:** `resume_cpu` after a manual `halt_cpu` may return an Xtensa-specific probe-rs error. Use `reset_target` as the reliable way to get the board running again.

---

## Tools

---

### `list_probes`

Enumerate all connected debug probes with their vendor ID, product ID, and hardware serial numbers.

**Parameters:** none

**Examples:**

1. Discover probes when setting up a new board pair:
```
list_probes()
→ [0] ESP JTAG — VID:303A PID:1001 — serial: 90:E5:B1:CB:CE:BC
→ [1] ESP JTAG — VID:303A PID:1001 — serial: 90:E5:B1:CB:CD:84
```

2. Identify a probe serial number after replug to update `devices.toml`:
```
list_probes()
→ note serial number, update probe_serial field in devices.toml
```

3. Verify both probes are present before starting a multi-board debug session:
```
list_probes()
→ confirm 2 entries before calling halt_cpu on both boards
```

---

### `list_boards`

Show all boards configured in `devices.toml` with their probe serial, target chip, and architecture classification.

**Parameters:** none

**Examples:**

1. Check board setup at the start of a debug session:
```
list_boards()
→ board1: target esp32s3 (non-Cortex-M), probe 90:E5:B1:CB:CE:BC
→ board2: target esp32s3 (non-Cortex-M), probe 90:E5:B1:CB:CD:84
```

2. Confirm a Cortex-M board is configured correctly before running `diagnose_hardfault`:
```
list_boards()
→ stm32: target STM32WL55JCIx (Cortex-M) ← safe to use all tools
```

3. Orient yourself in an unfamiliar lab setup:
```
list_boards()
→ understand which probe maps to which physical board before touching hardware
```

---

### `halt_cpu`

Halt the CPU and return the current program counter. Works on all architectures.

**Parameters:**
| Parameter | Required | Default | Description |
|-----------|----------|---------|-------------|
| `board` | no | first probe + default target | Board alias from `devices.toml` (e.g. `"board1"`) |

**Examples:**

1. Halt board1 to inspect its state:
```
halt_cpu(board="board1")
→ CPU halted. PC = 0x4037C2ED
```

2. Halt both boards simultaneously before a coordinated memory read:
```
halt_cpu(board="board1")
halt_cpu(board="board2")
```

3. Halt the default STM32 board (no config needed for single-board setups):
```
halt_cpu()
→ CPU halted. PC = 0x08004102
```

---

### `resume_cpu`

Resume CPU execution after halt. Works on all architectures. **ESP32 note:** if this returns an Xtensa error, use `reset_target` instead.

**Parameters:**
| Parameter | Required | Default | Description |
|-----------|----------|---------|-------------|
| `board` | no | first probe + default target | Board alias |

**Examples:**

1. Resume board1 after reading registers:
```
resume_cpu(board="board1")
```

2. Resume both boards after a coordinated halt:
```
resume_cpu(board="board1")
resume_cpu(board="board2")
```

3. Resume STM32 after fault diagnosis (single-board, no alias needed):
```
resume_cpu()
→ CPU resumed.
```

---

### `reset_target`

Soft reset the target, halt immediately after reset, read the post-reset PC, then resume. Reliable across all architectures including ESP32.

**Parameters:**
| Parameter | Required | Default | Description |
|-----------|----------|---------|-------------|
| `board` | no | first probe + default target | Board alias |

**Examples:**

1. Reset board1 after flashing new firmware:
```
reset_target(board="board1")
→ Target reset. PC after reset = 0x40000400. CPU resumed.
```

2. Recover an ESP32 board that is stuck after halt (resume failed):
```
reset_target(board="board2")
→ Target reset. PC after reset = 0x40000400. CPU resumed.
```

3. Verify reset vector address matches expected entry point:
```
reset_target(board="board1")
→ check that post-reset PC = expected bootloader/app entry address
```

---

### `read_registers`

Read CPU registers. Returns PC, SP, LR on all targets. On Cortex-M targets additionally returns R0–R12, xPSR, CONTROL/FAULTMASK/BASEPRI/PRIMASK. Halts the CPU during read, then resumes.

**Parameters:**
| Parameter | Required | Default | Description |
|-----------|----------|---------|-------------|
| `board` | no | first probe + default target | Board alias |

**Examples:**

1. Snapshot board1's register state during a hang:
```
read_registers(board="board1")
→ PC = 0x4037EB9B, SP = 0x3FCB3320, LR = 0x820A6925
```

2. Read full Cortex-M register set on STM32 to identify fault context:
```
read_registers()
→ PC, LR, SP, R0–R12, xPSR, EXTRA (CONTROL/FAULTMASK)
```

3. Check SP value to determine if a stack overflow may have occurred:
```
read_registers(board="board1")
→ compare SP against known stack bottom address in linker map
```

---

### `read_memory`

Read N bytes from any address and return a hex dump with ASCII representation.

**Parameters:**
| Parameter | Required | Default | Description |
|-----------|----------|---------|-------------|
| `board` | no | first probe + default target | Board alias |
| `address` | yes | — | Start address (decimal or hex) |
| `length` | no | 64 | Number of bytes to read |

**Examples:**

1. Read 64 bytes from ESP32-S3 SRAM to inspect a data structure:
```
read_memory(board="board1", address=0x3FCB3300, length=64)
→ 0x3FCB3300  DE AD BE EF ...
```

2. Read the STM32 vector table at flash start:
```
read_memory(address=0x08000000, length=32)
→ initial SP and reset vector visible in first 8 bytes
```

3. Inspect a suspect buffer on board2 during a protocol debug session:
```
read_memory(board="board2", address=0x3FC90000, length=128)
```

---

### `write_memory`

Write bytes to any target address. Halts the CPU during the write, then resumes. Use for fault injection, live variable patching, or clearing fault flags.

**Parameters:**
| Parameter | Required | Default | Description |
|-----------|----------|---------|-------------|
| `board` | no | first probe + default target | Board alias |
| `address` | yes | — | Start address |
| `data` | yes | — | Hex string, with or without spaces (e.g. `"DEADBEEF"` or `"DE AD BE EF"`) |

**Examples:**

1. Inject a fault by writing an invalid pointer to test crash handling:
```
write_memory(address=0x20000100, data="00006000")
→ Wrote 4 byte(s) to 0x20000100: 00 00 60 00
```

2. Live-patch a config flag on board1 without reflashing:
```
write_memory(board="board1", address=0x3FC90010, data="01")
```

3. Clear a stuck flag in RAM to unblock a frozen state machine:
```
write_memory(board="board2", address=0x3FCB0000, data="00000000")
```

---

### `read_call_stack`

Read the Cortex-M exception frame from the stack pointer — recovers PC, LR, R0–R3, R12, xPSR at the moment of the fault. **Cortex-M only.** On non-Cortex-M targets returns a guidance message pointing to the serial log backtrace.

**Parameters:**
| Parameter | Required | Default | Description |
|-----------|----------|---------|-------------|
| `board` | no | first probe + default target | Board alias |

**Examples:**

1. Read exception frame immediately after a HardFault on STM32:
```
read_call_stack()
→ PC = 0x08012345  ← instruction that faulted
→ LR = 0x08009ABC  ← return address
→ xPSR = exception number 3 (HardFault)
```

2. Identify the faulting function address for cross-reference with the map file:
```
read_call_stack()
→ SP+0x18  PC = 0x0801ABCD  ← look up in .map file
```

3. Confirm stack is intact (non-garbage xPSR value) before trusting the frame:
```
read_call_stack()
→ xPSR = 0x61000003  (exception 3, Thumb mode set — frame is valid)
```

---

### `diagnose_hardfault`

Read and decode Cortex-M fault status registers (HFSR, CFSR, BFAR, MMFAR) into a human-readable fault report. **Cortex-M only.** On non-Cortex-M targets returns a guidance message.

**Parameters:**
| Parameter | Required | Default | Description |
|-----------|----------|---------|-------------|
| `board` | no | first probe + default target | Board alias |

**CFSR flag reference:**

| Flag | Meaning |
|------|---------|
| `PRECISERR + BFARVALID` | Precise bus fault — BFAR holds the offending address |
| `IACCVIOL` | Jumped to non-executable address — check PC and LR |
| `DIVBYZERO` | Integer divide by zero — check PC for offending function |
| `STKERR` | Stack overflow on exception entry — check SP vs. stack bottom |
| `FORCED` in HFSR | Escalated fault — CFSR holds the real cause |

**Examples:**

1. Diagnose a HardFault on STM32 immediately after the anomaly is detected in serial log:
```
diagnose_hardfault()
→ CFSR = PRECISERR + BFARVALID
→ BFAR = 0x60000000  ← bus fault at this address
```

2. Distinguish a stack overflow from a bad pointer dereference:
```
diagnose_hardfault()
→ STKERR set → stack overflow on exception entry
→ cross-check: read_registers() → SP is below stack bottom
```

3. Decode an escalated fault — FORCED in HFSR means CFSR has the real cause:
```
diagnose_hardfault()
→ HFSR: FORCED
→ CFSR: DIVBYZERO  ← actual root cause
```

---

### `set_breakpoint`

Set a hardware breakpoint using the FPB unit. The CPU halts when PC reaches the given address. Works via probe-rs abstraction across architectures.

**Parameters:**
| Parameter | Required | Default | Description |
|-----------|----------|---------|-------------|
| `board` | no | first probe + default target | Board alias |
| `address` | yes | — | Address to break at |

**Examples:**

1. Break at a specific function entry point on STM32:
```
set_breakpoint(address=0x08004102)
→ Breakpoint set at 0x08004102 (6 HW breakpoint unit(s) available)
```

2. Break at the start of an ISR to verify it is being entered:
```
set_breakpoint(address=0x080123AB)
→ run firmware, wait for CPU to halt, then read_registers()
```

3. Set a breakpoint on board1 to catch a specific code path:
```
set_breakpoint(board="board1", address=0x4200ABCD)
```

---

### `clear_breakpoint`

Clear a hardware breakpoint by scanning FPB comparator registers. **Cortex-M only** (uses raw FPB register access — probe-rs clears FPB on session open so API-level clear is unreliable across sessions).

**Parameters:**
| Parameter | Required | Default | Description |
|-----------|----------|---------|-------------|
| `board` | no | first probe + default target | Board alias |
| `address` | yes | — | Address of the breakpoint to clear |

**Examples:**

1. Clear a breakpoint after the debug session is done:
```
clear_breakpoint(address=0x08004102)
→ Breakpoint cleared at 0x08004102 (FPB comparator unit 0)
```

2. Clear a breakpoint before flashing new firmware:
```
clear_breakpoint(address=0x080123AB)
```

3. Clear a breakpoint that may or may not be set (safe to call either way):
```
clear_breakpoint(address=0x4200ABCD)
→ No breakpoint found at 0x4200ABCD (may already be cleared)
```

---

### `set_watchpoint`

Set a DWT data watchpoint. The CPU halts when the watched address is read from or written to. **Cortex-M only** (uses DWT comparator registers).

**Parameters:**
| Parameter | Required | Default | Description |
|-----------|----------|---------|-------------|
| `board` | no | first probe + default target | Board alias |
| `address` | yes | — | Address to watch |
| `kind` | no | `"write"` | `"read"`, `"write"`, or `"read_write"` |

**Examples:**

1. Watch a global variable for unexpected writes on STM32:
```
set_watchpoint(address=0x20000200, kind="write")
→ Watchpoint set: write at 0x20000200 (DWT comparator unit 0)
→ run firmware — CPU halts on next write to this address
```

2. Watch `uwTick` for any access to catch SysTick ISR timing:
```
set_watchpoint(address=0x20000000, kind="read_write")
→ CPU halted in HAL_IncTick writing uwTick
```

3. Watch a shared buffer for reads to detect unexpected consumers:
```
set_watchpoint(address=0x200001A0, kind="read")
```

---

### `clear_watchpoint`

Remove a DWT data watchpoint. **Cortex-M only.**

**Parameters:**
| Parameter | Required | Default | Description |
|-----------|----------|---------|-------------|
| `board` | no | first probe + default target | Board alias |
| `address` | yes | — | Address of the watchpoint to clear |

**Examples:**

1. Clear a watchpoint after identifying the root cause:
```
clear_watchpoint(address=0x20000200)
→ Watchpoint cleared at 0x20000200 (DWT unit(s): 0)
```

2. Clear all watchpoints on a specific address before reflashing:
```
clear_watchpoint(address=0x20000000)
```

3. Clear a watchpoint that may or may not be active (safe to call either way):
```
clear_watchpoint(address=0x200001A0)
→ No active watchpoint at 0x200001A0
```

---

## Board Reference

Given the current `devices.toml`, the full board table is:

| Board | Probe serial | Target | Architecture | Alias |
|-------|-------------|--------|--------------|-------|
| `board1` | `90:E5:B1:CB:CE:BC` | `esp32s3` | non-Cortex-M | `board="board1"` |
| `board2` | `90:E5:B1:CB:CD:84` | `esp32s3` | non-Cortex-M | `board="board2"` |
| *(default)* | first available | `STM32WL55JCIx` | Cortex-M | omit `board` param |

To add a new board, add a `[board.<name>]` section to `devices.toml` with `probe_serial` and `target`. No rebuild required.

---

## Multi-Board Debug SOP

For a coordinated two-board debug session (e.g. one board transmits, the other receives):

```
1. list_probes()                          # confirm both probes visible
2. list_boards()                          # confirm aliases and targets
3. halt_cpu(board="board1")               # halt both
   halt_cpu(board="board2")
4. read_registers(board="board1")         # snapshot state of each
   read_registers(board="board2")
5. read_memory(board="board1", ...)       # inspect shared data structures
6. reset_target(board="board1")           # resume both via reset (ESP32)
   reset_target(board="board2")
```

For Cortex-M boards, add after step 4:
```
   diagnose_hardfault(board="stm32")      # decode fault registers
   read_call_stack(board="stm32")         # recover faulting PC
```

---

## Safety Constraints

| Constraint | Why |
|------------|-----|
| Never halt CPU for more than 1.5s on watchdog-enabled firmware | IWDG/task watchdog resets the target if CPU is halted too long (typically 2s timeout) |
| Always `halt_cpu` before `write_memory` | Prevents race conditions when patching live data structures |
| Always `clear_breakpoint` before flashing | Stale FPB state can confuse the new firmware's execution |
| Check `list_probes` before assuming probe order | Probe enumeration order can change on replug — always use `probe_serial` in `devices.toml` |
| `resume_cpu` may fail on ESP32 — use `reset_target` | probe-rs Xtensa halt/resume sequencing quirk; `reset_target` is reliable |
