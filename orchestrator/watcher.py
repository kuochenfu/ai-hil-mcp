#!/usr/bin/env python3
"""
AI-HIL Watcher — autonomous anomaly trigger for Claude Code.

Polls the serial port on a fixed interval. When an anomaly keyword is
detected, invokes `claude` CLI with a structured prompt so the full
Orchestrator SOP (defined in CLAUDE.md) runs automatically.

Usage:
    python orchestrator/watcher.py
    python orchestrator/watcher.py --port /dev/cu.usbmodem1303 --interval 30
"""

import argparse
import subprocess
import sys
import time
from datetime import datetime

import serial

# ── config ────────────────────────────────────────────────────────────────────

ANOMALY_KEYWORDS = [
    "hardfault", "hard fault", "panic", "assert",
    "watchdog", "iwdg", "stack overflow",
]

CLAUDE_PROMPT_TEMPLATE = """\
ANOMALY DETECTED on {port} at {timestamp}.

Anomaly lines from serial log:
{anomaly_lines}

Follow the Orchestrator SOP in CLAUDE.md exactly:
1. Triage — classify the anomaly type from the lines above
2. Diagnosis — run the appropriate JTAG diagnosis tools in parallel
3. Remediation — fix the source code
4. Build & Flash — rebuild and flash to the board
5. Verification — confirm clean boot via serial log
6. Record — append the bug to the Known Bug Record in CLAUDE.md

Do not ask for confirmation. Work autonomously through all steps.
"""

# ── serial reader ─────────────────────────────────────────────────────────────

def read_lines(port: str, baud: int, timeout_s: int) -> list[str]:
    try:
        ser = serial.Serial(port, baud, timeout=1)
        lines = []
        deadline = time.time() + timeout_s
        while time.time() < deadline:
            line = ser.readline().decode(errors="replace").strip()
            if line:
                lines.append(line)
        ser.close()
        return lines
    except serial.SerialException as e:
        print(f"[watcher] Serial error: {e}", file=sys.stderr)
        return []


def find_anomalies(lines: list[str]) -> list[str]:
    return [
        l for l in lines
        if any(k in l.lower() for k in ANOMALY_KEYWORDS)
    ]


# ── claude trigger ────────────────────────────────────────────────────────────

def trigger_claude(port: str, anomaly_lines: list[str], repo_dir: str) -> None:
    timestamp = datetime.now().strftime("%Y-%m-%d %H:%M:%S")
    prompt = CLAUDE_PROMPT_TEMPLATE.format(
        port=port,
        timestamp=timestamp,
        anomaly_lines="\n".join(f"  {l}" for l in anomaly_lines),
    )

    print(f"\n[watcher] *** ANOMALY DETECTED at {timestamp} ***")
    for l in anomaly_lines:
        print(f"[watcher]   {l}")
    print("[watcher] Triggering Claude Code orchestrator...\n")

    subprocess.run(
        ["claude", "--print", prompt],
        cwd=repo_dir,
        check=False,
    )


# ── main loop ─────────────────────────────────────────────────────────────────

def main() -> None:
    parser = argparse.ArgumentParser(description="AI-HIL anomaly watcher")
    parser.add_argument("--port", default="/dev/cu.usbmodem1303")
    parser.add_argument("--baud", type=int, default=115200)
    parser.add_argument("--interval", type=int, default=30,
                        help="Poll interval in seconds (default: 30)")
    parser.add_argument("--read-window", type=int, default=8,
                        help="Serial read window per poll in seconds (default: 8)")
    parser.add_argument("--repo", default="/Users/chenfu/Labs/ai-hil-mcp",
                        help="Path to ai-hil-mcp repo (for claude cwd)")
    args = parser.parse_args()

    print(f"[watcher] Starting — port={args.port} interval={args.interval}s")
    print(f"[watcher] Anomaly keywords: {ANOMALY_KEYWORDS}")
    print("[watcher] Press Ctrl+C to stop.\n")

    cooldown_until = 0.0  # don't re-trigger during active remediation

    while True:
        now = time.time()

        if now < cooldown_until:
            remaining = int(cooldown_until - now)
            print(f"[watcher] In cooldown — {remaining}s remaining", end="\r")
            time.sleep(5)
            continue

        print(f"[watcher] [{datetime.now().strftime('%H:%M:%S')}] Polling {args.port}...", end="\r")
        lines = read_lines(args.port, args.baud, args.read_window)
        anomalies = find_anomalies(lines)

        if anomalies:
            trigger_claude(args.port, anomalies, args.repo)
            # Cooldown: give Claude time to remediate before re-triggering
            # Typical fix cycle: build (~60s) + flash (~15s) + verify (~15s) = ~2min
            cooldown_until = time.time() + 180
            print(f"\n[watcher] Cooldown active for 180s (remediation in progress)")
        else:
            time.sleep(args.interval)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        print("\n[watcher] Stopped.")
