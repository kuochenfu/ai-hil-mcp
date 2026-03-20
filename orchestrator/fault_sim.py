#!/usr/bin/env python3
"""
AI-HIL Fault Simulator — creates a virtual serial port (PTY) that streams
realistic firmware output, then injects a HardFault to trigger the watcher.

Usage:
    python orchestrator/fault_sim.py

The script prints the PTY device path (e.g. /dev/ttys007).
Point the watcher at that path:
    python orchestrator/watcher.py --port /dev/ttys007

The simulator will:
  1. Stream normal LoRa PING/PONG output for ~10 seconds
  2. Inject a HardFault message
  3. Keep the PTY open so the watcher can detect and trigger claude
"""

import os
import pty
import tty
import time
import sys
import threading

NORMAL_LINES = [
    "Tx PING\r\n",
    "TxDone — listening\r\n",
    "RxDone  len=64 RSSI=-87 SNR=12 payload=PING\r\n",
    "RxDone  unexpected payload — restarting RX\r\n",
    "RxTimeout\r\n",
]

FAULT_LINES = [
    "\r\n*** SW3: HardFault injection test ***\r\n",
    "HardFault detected\r\n",
    "CFSR=0x00000200 HFSR=0x40000000\r\n",
    "PC=0x08004ABC LR=0x080040CD SP=0x20003F40\r\n",
    "system halted\r\n",
]


def write_line(fd: int, line: str) -> None:
    os.write(fd, line.encode())


def run(fault_after_s: float) -> None:
    master_fd, slave_fd = pty.openpty()
    slave_path = os.ttyname(slave_fd)
    # Set master to raw mode so the line discipline doesn't mangle bytes.
    # This lets the watcher open the slave with serial.Serial() (which calls
    # tcsetattr) without disrupting data flow.
    tty.setraw(master_fd)

    print(f"[sim] Virtual serial port: {slave_path}")
    print(f"[sim] Run watcher with:  python orchestrator/watcher.py --port {slave_path}")
    print(f"[sim] HardFault fires in {fault_after_s}s")
    print(f"[sim] Streaming normal output...")
    sys.stdout.flush()

    start = time.time()
    idx = 0
    fault_injected = False

    try:
        while True:
            elapsed = time.time() - start

            if not fault_injected and elapsed >= fault_after_s:
                print("\n[sim] Injecting HardFault...")
                for line in FAULT_LINES:
                    write_line(master_fd, line)
                    time.sleep(0.05)
                fault_injected = True
                print("[sim] Fault injected. Waiting for watcher to trigger...")

            # Keep streaming normal output so the port stays alive
            write_line(master_fd, NORMAL_LINES[idx % len(NORMAL_LINES)])
            idx += 1
            time.sleep(1.5)

    except KeyboardInterrupt:
        print("\n[sim] Stopped.")
    finally:
        os.close(master_fd)
        os.close(slave_fd)


if __name__ == "__main__":
    import argparse
    parser = argparse.ArgumentParser()
    parser.add_argument("--fault-after", type=float, default=10.0,
                        help="Seconds before injecting fault (default: 10)")
    args = parser.parse_args()
    run(args.fault_after)
