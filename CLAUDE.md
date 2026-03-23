# CLAUDE.md — AI-HIL Embedded Dev Automation

This file is the operating brain for Claude Code on this repository.
Follow the SOPs below exactly when working with physical hardware.

---

## Project Overview

**AI-HIL (AI-Hardware-in-the-Loop)** gives Claude Code the ability to perceive, act on,
and validate physical embedded hardware via MCP servers.

## Active MCP Servers

| Server | Tools | Binary |
|--------|-------|--------|
| `serial-mcp` | `list_serial_ports`, `read_serial_log`, `send_serial_command` | `serial-mcp-rs` |
| `jtag-mcp` | `halt_cpu`, `resume_cpu`, `read_registers`, `read_memory`, `read_call_stack`, `diagnose_hardfault` | `jtag-mcp-rs` |
| `build-flash-mcp` | `build_firmware`, `clean_build`, `get_build_size`, `flash_firmware` | `build-flash-mcp-rs` |
| `ppk2-mcp` | `find_ppk2`, `measure_current`, `profile_power_states`, `measure_with_pin_trigger`, `estimate_battery_life`, `set_dut_power`, `get_metadata` | `ppk2-mcp-rs` |

## Target Hardware

- **STM32WL55JC** (NUCLEO-WL55JC1) — Sub-GHz LoRa, ultra-low-power
- Serial port: `/dev/cu.usbmodem1303` @ 115200 baud
- Firmware project: `/Users/chenfu/Labs/stm_projects/synapse-lora/CM4`, preset `Debug`
- Debugger: ST-Link V3 via `jtag-mcp`
- **Nordic PPK2** — power measurement via `ppk2-mcp` (auto-detect port with `find_ppk2`)

---

## Safety Constraints — ALWAYS ENFORCE

- **Never** modify ISR handlers without reading call stack first (`read_call_stack`)
- **Always** call `halt_cpu()` before any flash operation
- **Never** flash if build returned errors
- **Wait 3s** after flash before reading serial (board needs time to boot)
- Watchdog timeout is 2s — do not halt CPU for more than 1.5s during live diagnosis
- If `diagnose_hardfault` shows `FORCED` in HFSR, always check CFSR for root cause before touching code
- **PPK2**: Never set `source_meter` voltage above the DUT's rated VDD (STM32WL55: max 3.6V → use 3300 mV)
- **PPK2**: `measure_current` automatically disables DUT power after the measurement — no manual cleanup needed

---

## Orchestrator SOP

When triggered (manually or by watcher), execute this loop in order.
Do not skip steps. Do not ask for confirmation between steps unless blocked.

### Step 1 — Triage

```
read_serial_log(port="/dev/cu.usbmodem1303", baud=115200, lines=50, timeout_s=8)
```

Classify the result:

| Pattern in log | Classification | Go to |
|---------------|---------------|-------|
| `HardFault`, `hard fault` | HardFault | Step 2A |
| `panic`, `assert` | Panic / Assert | Step 2B |
| `watchdog`, `IWDG` | Watchdog Reset | Step 2C |
| `stack overflow` | Stack Overflow | Step 2A |
| No output / timeout | Board hang or dead | Step 2D |
| Clean output, no anomaly | Healthy | Stop — log "No anomaly detected" |

### Step 2A — HardFault / Stack Overflow Diagnosis

Run these in parallel:

```
diagnose_hardfault()          ← decode HFSR + CFSR + fault address
read_call_stack()             ← exception frame at SP
read_registers()              ← full register snapshot
```

Interpret:
- `PRECISERR + BFARVALID` → precise bus fault, check BFAR address
- `IACCVIOL` → jumped to invalid address, check PC and LR
- `DIVBYZERO` → integer divide by zero, check PC for offending function
- `STKERR` → stack overflow confirmed, check SP vs stack bottom
- `FORCED` in HFSR → escalated fault, CFSR holds real cause

Locate the faulting function: PC from exception frame (SP+0x18) → cross-reference with map file or ELF symbols.

### Step 2B — Panic / Assert Diagnosis

```
read_serial_log(lines=100)    ← capture full panic output
read_registers()              ← confirm CPU state
```

Extract file, line number, and condition from the panic message.

### Step 2C — Watchdog Reset Diagnosis

```
read_serial_log(lines=100)    ← look for last activity before reset
read_registers()              ← check if CPU is in unexpected state
```

Look for long-running loops, blocking waits, or missing `HAL_IWDG_Refresh()` calls.

### Step 2D — Board Hang (No Output)

```
halt_cpu()
read_registers()              ← where is PC stuck?
read_call_stack()
```

Check if CPU is spinning in a tight loop, blocked on semaphore, or in infinite fault loop.

### Step 3 — Remediation

Based on diagnosis:
1. Identify the source file and function at fault
2. Read the relevant source file(s) before making any changes
3. Apply the minimal fix — do not refactor unrelated code
4. If touching an ISR: re-read call stack first, ensure fix does not affect interrupt timing

### Step 4 — Build & Flash

```
build_firmware(
  project_path="/Users/chenfu/Labs/stm_projects/synapse-lora/CM4",
  preset="Debug"
)
```

If build fails: fix compile errors, do not flash. Go back to Step 3.

```
flash_firmware(
  project_path="/Users/chenfu/Labs/stm_projects/synapse-lora/CM4"
)
```

Wait 3 seconds after flash completes.

### Step 5 — Verification

```
read_serial_log(port="/dev/cu.usbmodem1303", lines=30, timeout_s=10)
```

Pass criteria:
- No anomaly keywords in output
- Expected boot banner present (e.g. `System Initialization`, `Tx PING`)
- Board producing output (not silent)

If FAIL: go back to Step 2 with the new serial output as context.
If PASS: go to Step 6.

### Step 6 — Record to Known Bug Record

Append to the Known Bug Record section below:

```
### [YYYY-MM-DD] <short title>
- **Symptom:** <what the serial log / watchdog showed>
- **Root cause:** <CFSR flags, function name, line>
- **Fix:** <what was changed and why>
- **Verified:** clean boot confirmed
```

---

## Known Bug Record

<!-- Orchestrator appends entries here after each verified fix -->

### [2026-03-19] HardFault — PRECISERR on invalid memory read
- **Symptom:** HardFault detected in serial log during LoRa RX callback
- **Root cause:** CFSR=PRECISERR+BFARVALID, BFAR=0x60000000 — deliberate fault injection test
- **Fix:** Test only — reflashed clean firmware, all fault bits cleared
- **Verified:** clean boot confirmed, LoRa PING/PONG traffic resumed

---

## Architecture Reference

```
Brain:           Claude Code CLI + this CLAUDE.md
Nervous System:  MCP servers (Rust binaries)
Perception:      serial-mcp (UART logs) · jtag-mcp (registers, memory, faults)
Action:          build-flash-mcp (CMake + OpenOCD)
```

### Closed-Loop Flow

```
Triage → Diagnosis → Remediation → Build & Flash → Verification → Known Bug Record
  ▲                                                      │
  └──────────────────── FAIL ────────────────────────────┘
```
