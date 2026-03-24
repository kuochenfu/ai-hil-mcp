"""
vision-mcp test runner
======================
Tests each tool individually. Imports server functions directly — no MCP transport needed.

Usage:
    python tests.py          # run all tests
    python tests.py 1        # run test 1 only
    python tests.py 1 3 5    # run tests 1, 3, and 5
    python tests.py --list   # show all test names

Output:
    [PASS]        — automated check passed
    [FAIL]        — automated check failed
    [MANUAL]      — result saved to /tmp/vision_test_*.jpg, inspect visually
    [SKIP]        — skipped (missing hardware/API key)
    [WARN]        — passed with a caveat
"""

import sys
import os
import base64
import importlib.util

# ── Load server module from sibling file ─────────────────────────────────────
_SERVER = os.path.join(os.path.dirname(__file__), "server.py")
spec = importlib.util.spec_from_file_location("server", _SERVER)
srv  = importlib.util.module_from_spec(spec)
spec.loader.exec_module(srv)

# ── Helpers ───────────────────────────────────────────────────────────────────

_PASS  = "\033[32m[PASS]\033[0m"
_FAIL  = "\033[31m[FAIL]\033[0m"
_MAN   = "\033[33m[MANUAL]\033[0m"
_SKIP  = "\033[90m[SKIP]\033[0m"
_WARN  = "\033[35m[WARN]\033[0m"

_saved_frames = []


def _save_frame(tag: str, jpeg_b64: str) -> str:
    path = f"/tmp/vision_test_{tag}.jpg"
    with open(path, "wb") as f:
        f.write(base64.b64decode(jpeg_b64))
    _saved_frames.append(path)
    return path


def _extract_b64(result: str) -> str | None:
    for line in result.splitlines():
        if line.startswith("FRAME_BASE64:"):
            return line[len("FRAME_BASE64:"):]
    return None


def _reset_state():
    """Reset server state to defaults between tests."""
    srv._state.update({
        "camera_index": 0, "width": 1920, "height": 1080,
        "pan": 0.0, "tilt": 0.0, "zoom": 1.0,
        "brightness": 0, "contrast": 1.0, "saturation": 1.0, "sharpness": 0,
    })


def _header(n: int, name: str, requires: str = ""):
    req = f"  \033[90mrequires: {requires}\033[0m" if requires else ""
    print(f"\n{'─'*60}")
    print(f"Test {n:02d}: {name}{req}")
    print('─'*60)


# ── Tests ─────────────────────────────────────────────────────────────────────

def test_01_list_cameras():
    _header(1, "list_cameras", "camera connected + macOS permission granted")
    result = srv.list_cameras()
    print(result)
    if "No cameras" in result:
        print(f"{_FAIL} No cameras found. Check macOS camera permission.")
        return False
    if "[0]" in result:
        print(f"{_PASS} Camera index 0 detected.")
        return True
    print(f"{_WARN} Cameras found but not at index 0 — check index before proceeding.")
    return True


def test_02_get_camera_info():
    _header(2, "get_camera_info — verify defaults")
    _reset_state()
    result = srv.get_camera_info()
    print(result)
    checks = {
        "Camera index : 0":  "camera_index default",
        "Resolution   : 1920x1080": "resolution default",
        "Pan          : +0.00": "pan default",
        "Zoom         : 1.00x": "zoom default",
        "Brightness   : +0":   "brightness default",
    }
    failed = [label for expected, label in checks.items() if expected not in result]
    if failed:
        print(f"{_FAIL} Wrong defaults: {failed}")
        return False
    print(f"{_PASS} All defaults correct.")
    return True


def test_03_set_resolution():
    _header(3, "set_resolution — switch to 1280x720 and back")
    r1 = srv.set_resolution(1280, 720)
    print(f"→ 1280x720: {r1}")
    if "1280x720" not in r1 and "snapped" not in r1:
        print(f"{_FAIL} Unexpected response: {r1}")
        return False

    r2 = srv.set_resolution(1920, 1080)
    print(f"→ 1920x1080: {r2}")
    if "1920x1080" not in r2:
        print(f"{_FAIL} Could not restore 1920x1080.")
        return False

    print(f"{_PASS} Resolution switching works.")
    return True


def test_04_set_ptz():
    _header(4, "set_ptz — set values and verify state")
    _reset_state()
    srv.set_ptz(pan=0.5, tilt=-0.3, zoom=2.0)
    info = srv.get_camera_info()
    ok = "Pan          : +0.50" in info and \
         "Tilt         : -0.30" in info and \
         "Zoom         : 2.00x" in info
    print(info)
    if not ok:
        print(f"{_FAIL} PTZ state not updated correctly.")
        return False
    print(f"{_PASS} PTZ state persisted.")
    _reset_state()
    return True


def test_05_adjust_image():
    _header(5, "adjust_image — set values and verify state")
    _reset_state()
    srv.adjust_image(brightness=40, contrast=1.5, saturation=0.8, sharpness=3)
    info = srv.get_camera_info()
    ok = "Brightness   : +40" in info and \
         "Contrast     : 1.50" in info and \
         "Saturation   : 0.80" in info and \
         "Sharpness    : 3"    in info
    print(info)
    if not ok:
        print(f"{_FAIL} Image adjustment state not updated correctly.")
        return False
    print(f"{_PASS} Image adjustment state persisted.")
    _reset_state()
    return True


def test_06_capture_frame_basic():
    _header(6, "capture_frame — basic capture at 1080p", "camera")
    _reset_state()
    result = srv.capture_frame()
    if result.startswith("ERROR"):
        print(f"{_FAIL} {result}")
        return False
    b64 = _extract_b64(result)
    if not b64:
        print(f"{_FAIL} No base64 data in result:\n{result}")
        return False
    path = _save_frame("06_basic_1080p", b64)
    size_line = [l for l in result.splitlines() if l.startswith("SIZE")]
    print(f"Result: {size_line[0] if size_line else result[:80]}")
    print(f"{_MANUAL} Saved → {path}  (open and verify it's a valid image)")
    return True

_MANUAL = _MAN  # alias


def test_07_capture_frame_ptz():
    _header(7, "capture_frame — with PTZ (zoom 2×, pan right)", "camera")
    _reset_state()
    srv.set_ptz(pan=0.4, tilt=0.0, zoom=2.0)
    result = srv.capture_frame()
    if result.startswith("ERROR"):
        print(f"{_FAIL} {result}")
        _reset_state()
        return False
    b64 = _extract_b64(result)
    path = _save_frame("07_ptz_zoom2_pan_right", b64)
    print(f"{_MANUAL} Saved → {path}")
    print("        Verify: image should be zoomed in and shifted right vs test_06.")
    _reset_state()
    return True


def test_08_capture_frame_brightness():
    _header(8, "capture_frame — image adjustments (brightness +60, sharpness 5)", "camera")
    _reset_state()
    srv.adjust_image(brightness=60, sharpness=5)
    result = srv.capture_frame()
    if result.startswith("ERROR"):
        print(f"{_FAIL} {result}")
        _reset_state()
        return False
    b64 = _extract_b64(result)
    path = _save_frame("08_bright_sharp", b64)
    print(f"{_MANUAL} Saved → {path}")
    print("        Verify: image should be noticeably brighter than test_06.")
    _reset_state()
    return True


def test_09_detect_led_state_no_board():
    _header(9, "detect_led_state — no board (expect 'no LEDs detected')", "camera, no board in view")
    _reset_state()
    result = srv.detect_led_state()
    print(result)
    if "ERROR" in result:
        print(f"{_FAIL} {result}")
        return False
    print(f"{_MANUAL} Verify: if no board is in view, should report 0 LEDs or low confidence.")
    return True


def test_10_detect_led_state_with_board():
    _header(10, "detect_led_state — NUCLEO board in view", "camera + NUCLEO board powered on")
    print("Setup: point camera at the NUCLEO-WL55JC1 board with LEDs on.")
    input("Press Enter when ready...")
    _reset_state()
    srv.set_ptz(zoom=1.5)
    result = srv.detect_led_state(region_hint="top-right")
    print(result)
    if "ERROR" in result:
        print(f"{_FAIL} {result}")
        _reset_state()
        return False
    if "LED" in result and ("GREEN" in result or "BLUE" in result or "RED" in result):
        print(f"{_PASS} At least one LED detected.")
    else:
        print(f"{_MANUAL} No LED color found — check lighting and camera framing.")
    _reset_state()
    return True


def test_11_read_display():
    _header(11, "read_display — OCR from LCD/OLED", "camera + board with a display")
    print("Setup: point camera at the board's LCD or OLED display.")
    input("Press Enter when ready (or Ctrl+C to skip)...")
    _reset_state()
    srv.set_ptz(zoom=2.0)
    result = srv.read_display()
    print(result)
    if result.startswith("ERROR"):
        print(f"{_FAIL} {result}")
        _reset_state()
        return False
    if "OCR" in result and "No text" not in result:
        print(f"{_PASS} Text extracted by OCR.")
    else:
        print(f"{_MANUAL} Low OCR confidence — verify manually. Consider better lighting or zoom.")
    _reset_state()
    return True


def test_12_detect_jumper():
    _header(12, "detect_jumper — JP1 presence check", "camera + board with a jumper visible")
    print("Setup: point camera at the jumper header (e.g. JP1 on NUCLEO).")
    print("       The jumper should be clearly visible.")
    input("Press Enter when ready (or Ctrl+C to skip)...")
    _reset_state()
    srv.set_ptz(zoom=2.0)
    # Test 'present' case
    result = srv.detect_jumper(expected="present")
    print(f"With jumper installed:\n{result}")
    if "MISMATCH" in result:
        print(f"{_WARN} Jumper not detected — check framing and lighting.")
    elif "PRESENT" in result:
        print(f"{_PASS} Jumper detected as present.")
    else:
        print(f"{_MANUAL} Check result above.")

    print("\nNow REMOVE the jumper and press Enter...")
    input()
    result2 = srv.detect_jumper(expected="absent")
    print(f"Without jumper:\n{result2}")
    if "ABSENT" in result2 and "MISMATCH" not in result2:
        print(f"{_PASS} Correctly detected jumper absent after removal.")
    else:
        print(f"{_MANUAL} Check result above.")
    _reset_state()
    return True


def test_13_check_board():
    _header(13, "check_board — PCB presence + orientation", "camera + PCB in view")
    print("Test A: board in view")
    input("Point camera at board, then press Enter...")
    _reset_state()
    result = srv.check_board()
    print(f"Board in view:\n{result}")
    if "PRESENT" in result:
        print(f"{_PASS} Board detected.")
    elif "NOT DETECTED" in result:
        print(f"{_WARN} Board not detected — ensure PCB green is visible and well-lit.")
    else:
        print(f"{_MANUAL} Check result above.")

    print("\nTest B: no board in view")
    input("Remove board from frame, then press Enter...")
    result2 = srv.check_board()
    print(f"No board:\n{result2}")
    if "NOT DETECTED" in result2:
        print(f"{_PASS} Correctly reports no board when absent.")
    else:
        print(f"{_MANUAL} Check result above.")
    return True


def test_14_detect_motion():
    _header(14, "detect_motion — 3 second window", "camera")

    print("Test A: no motion (keep scene still)")
    input("Press Enter to start 3s monitoring (keep everything still)...")
    result = srv.detect_motion(duration_s=3.0, sensitivity=20)
    print(result)
    if "NO MOTION" in result:
        print(f"{_PASS} No motion correctly reported.")
    else:
        print(f"{_WARN} Motion detected in still scene — check sensitivity or vibration.")

    print("\nTest B: motion (wave hand in front of camera during monitoring)")
    input("Press Enter to start 3s monitoring (wave your hand in frame)...")
    result2 = srv.detect_motion(duration_s=3.0, sensitivity=20)
    print(result2)
    if "motion event" in result2:
        print(f"{_PASS} Motion correctly detected.")
    else:
        print(f"{_WARN} No motion detected — try lower sensitivity or larger movement.")
    return True


def test_15_read_qr_code():
    _header(15, "read_qr_code — QR code detection", "camera + QR code in view")
    print("Setup: show a QR code to the camera.")
    print("       Tip: open a QR code on your phone screen and hold it in front of the camera.")
    print("       Or use the NUCLEO board label if it has a barcode.")
    input("Press Enter when ready (or Ctrl+C to skip)...")
    _reset_state()
    result = srv.read_qr_code()
    print(result)
    if result.startswith("ERROR"):
        print(f"{_FAIL} {result}")
        return False
    if "Detected" in result:
        print(f"{_PASS} QR code decoded successfully.")
    else:
        print(f"{_MANUAL} Not decoded — try set_ptz(zoom=2.0) and better lighting.")
    return True


def test_16_set_focus():
    _header(16, "set_focus — AVFoundation auto/manual", "camera + pyobjc-framework-AVFoundation")
    r1 = srv.set_focus("auto")
    print(f"set_focus('auto'):   {r1}")
    r2 = srv.set_focus("manual")
    print(f"set_focus('manual'): {r2}")
    r3 = srv.set_focus("bad_value")
    print(f"set_focus('bad'):    {r3}")
    if "ERROR" in r3 or "must be" in r3:
        print(f"{_PASS} Invalid mode correctly rejected.")
    if "AVFoundation" in r1 or "Focus set" in r1:
        print(f"{_PASS} AVFoundation focus control responded.")
    else:
        print(f"{_MANUAL} Check result — AVFoundation may need an active session.")
    return True


def test_17_analyze_frame():
    _header(17, "analyze_frame — Claude vision API", "camera + ANTHROPIC_API_KEY")
    if not srv._api_available():
        print(f"{_SKIP} ANTHROPIC_API_KEY not set.")
        return None
    _reset_state()
    result = srv.analyze_frame(prompt="Describe what you see in one sentence.")
    print(result)
    if result.startswith("ERROR"):
        print(f"{_FAIL} {result}")
        return False
    print(f"{_PASS} Claude vision responded.")
    return True


# ── Registry ──────────────────────────────────────────────────────────────────

TESTS = [
    (1,  "list_cameras",                   test_01_list_cameras),
    (2,  "get_camera_info (defaults)",      test_02_get_camera_info),
    (3,  "set_resolution",                  test_03_set_resolution),
    (4,  "set_ptz",                         test_04_set_ptz),
    (5,  "adjust_image",                    test_05_adjust_image),
    (6,  "capture_frame (basic 1080p)",     test_06_capture_frame_basic),
    (7,  "capture_frame (PTZ zoom+pan)",    test_07_capture_frame_ptz),
    (8,  "capture_frame (brightness)",      test_08_capture_frame_brightness),
    (9,  "detect_led_state (no board)",     test_09_detect_led_state_no_board),
    (10, "detect_led_state (with board)",   test_10_detect_led_state_with_board),
    (11, "read_display (OCR)",              test_11_read_display),
    (12, "detect_jumper",                   test_12_detect_jumper),
    (13, "check_board",                     test_13_check_board),
    (14, "detect_motion",                   test_14_detect_motion),
    (15, "read_qr_code",                    test_15_read_qr_code),
    (16, "set_focus",                       test_16_set_focus),
    (17, "analyze_frame (Claude vision)",   test_17_analyze_frame),
]

# ── Entry point ───────────────────────────────────────────────────────────────

if __name__ == "__main__":
    args = sys.argv[1:]

    if "--list" in args:
        print("\nAvailable tests:")
        for n, name, _ in TESTS:
            print(f"  {n:2d}. {name}")
        sys.exit(0)

    selected = [int(a) for a in args if a.isdigit()]
    run = [(n, name, fn) for n, name, fn in TESTS if not selected or n in selected]

    print(f"\nvision-mcp test runner — running {len(run)} test(s)")
    print(f"Frames saved to /tmp/vision_test_*.jpg\n")

    results = {}
    for n, name, fn in run:
        try:
            results[n] = fn()
        except KeyboardInterrupt:
            print(f"\n{_SKIP} Skipped by user.")
            results[n] = None
        except Exception as e:
            print(f"{_FAIL} Exception: {e}")
            results[n] = False

    # ── Summary ───────────────────────────────────────────────────────────────
    print(f"\n{'═'*60}")
    print("SUMMARY")
    print('═'*60)
    for n, name, _ in run:
        r = results.get(n)
        status = _PASS if r is True else _FAIL if r is False else _SKIP if r is None else _MAN
        print(f"  {n:2d}. {status} {name}")

    if _saved_frames:
        print(f"\nSaved frames:")
        for p in _saved_frames:
            print(f"  {p}")
        print("\nOpen them with:")
        print(f"  open {' '.join(_saved_frames)}")
