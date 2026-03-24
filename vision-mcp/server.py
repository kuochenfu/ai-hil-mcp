from fastmcp import FastMCP
import cv2
import base64
import numpy as np
import os

mcp = FastMCP("vision-mcp")

# ── Persistent camera state ───────────────────────────────────────────────────
# All capture and analyze tools respect this state.

_state = {
    "camera_index": 0,
    "width": 1920,
    "height": 1080,
    # Software PTZ (applied post-capture via crop+resize)
    "pan":  0.0,   # -1.0 (full left) … +1.0 (full right)
    "tilt": 0.0,   # -1.0 (full up)   … +1.0 (full down)
    "zoom": 1.0,   # 1.0 = no zoom, 4.0 = 4× digital zoom
    # Post-capture image adjustments
    "brightness": 0,    # -100 … +100
    "contrast":   1.0,  # 0.0 … 3.0  (1.0 = no change)
    "saturation": 1.0,  # 0.0 … 3.0
    "sharpness":  0,    # 0 … 10
}

# ── Anthropic client (lazy, fallback only) ────────────────────────────────────

_client = None


def _get_client():
    global _client
    if _client is None:
        import anthropic
        _client = anthropic.Anthropic()
    return _client


def _api_available() -> bool:
    return bool(os.environ.get("ANTHROPIC_API_KEY"))


# ── Camera capture ────────────────────────────────────────────────────────────

def _capture_bgr(camera_index: int, width: int, height: int) -> np.ndarray:
    cap = cv2.VideoCapture(camera_index)
    if not cap.isOpened():
        raise RuntimeError(f"Cannot open camera index {camera_index}")
    cap.set(cv2.CAP_PROP_FRAME_WIDTH, width)
    cap.set(cv2.CAP_PROP_FRAME_HEIGHT, height)
    for _ in range(3):
        cap.read()
    ret, frame = cap.read()
    cap.release()
    if not ret or frame is None:
        raise RuntimeError("Failed to capture frame")
    return frame


def _apply_ptz(frame: np.ndarray, pan: float, tilt: float, zoom: float) -> np.ndarray:
    """Crop frame to simulate pan/tilt/zoom, then resize back to original dimensions."""
    if zoom == 1.0 and pan == 0.0 and tilt == 0.0:
        return frame
    h, w = frame.shape[:2]
    # Crop size shrinks as zoom increases
    crop_w = int(w / zoom)
    crop_h = int(h / zoom)
    # Center of crop, shifted by pan/tilt
    cx = w // 2 + int(pan * (w - crop_w) / 2)
    cy = h // 2 + int(tilt * (h - crop_h) / 2)
    # Clamp so crop stays within frame
    x1 = max(0, min(cx - crop_w // 2, w - crop_w))
    y1 = max(0, min(cy - crop_h // 2, h - crop_h))
    cropped = frame[y1:y1 + crop_h, x1:x1 + crop_w]
    return cv2.resize(cropped, (w, h), interpolation=cv2.INTER_LINEAR)


def _apply_image_adjustments(frame: np.ndarray, brightness: int, contrast: float,
                              saturation: float, sharpness: int) -> np.ndarray:
    """Apply brightness/contrast/saturation/sharpness post-capture."""
    out = frame.astype(np.float32)

    # Brightness (+/-) and contrast (scale)
    out = out * contrast + brightness
    out = np.clip(out, 0, 255)

    # Saturation via HSV
    if saturation != 1.0:
        bgr = out.astype(np.uint8)
        hsv = cv2.cvtColor(bgr, cv2.COLOR_BGR2HSV).astype(np.float32)
        hsv[:, :, 1] = np.clip(hsv[:, :, 1] * saturation, 0, 255)
        out = cv2.cvtColor(hsv.astype(np.uint8), cv2.COLOR_HSV2BGR).astype(np.float32)

    out = out.astype(np.uint8)

    # Sharpness via unsharp mask
    if sharpness > 0:
        strength = sharpness * 0.3
        blurred = cv2.GaussianBlur(out, (0, 0), 3)
        out = cv2.addWeighted(out, 1 + strength, blurred, -strength, 0)

    return out


def _get_processed_frame() -> np.ndarray:
    """Capture + apply current PTZ and image adjustments."""
    s = _state
    frame = _capture_bgr(s["camera_index"], s["width"], s["height"])
    frame = _apply_ptz(frame, s["pan"], s["tilt"], s["zoom"])
    frame = _apply_image_adjustments(
        frame, s["brightness"], s["contrast"], s["saturation"], s["sharpness"]
    )
    return frame


def _bgr_to_jpeg(frame: np.ndarray, quality: int = 90) -> bytes:
    _, buf = cv2.imencode(".jpg", frame, [cv2.IMWRITE_JPEG_QUALITY, quality])
    return buf.tobytes()


def _frame_to_base64(jpeg_bytes: bytes) -> str:
    return base64.standard_b64encode(jpeg_bytes).decode()


# ── OpenCV LED detection ──────────────────────────────────────────────────────

_LED_COLORS = [
    ("red",    np.array([0,   120, 180]), np.array([8,   255, 255])),
    ("red",    np.array([172, 120, 180]), np.array([180, 255, 255])),
    ("green",  np.array([40,  80,  120]), np.array([90,  255, 255])),
    ("blue",   np.array([100, 80,  120]), np.array([135, 255, 255])),
    ("yellow", np.array([20,  120, 180]), np.array([35,  255, 255])),
    ("orange", np.array([9,   120, 180]), np.array([19,  255, 255])),
    ("white",  np.array([0,   0,   220]), np.array([180, 30,  255])),
]

_MIN_AREA = 8
_MAX_AREA = 4000
_CONFIDENCE_THRESHOLD = 0.6


def _detect_leds_opencv(frame: np.ndarray, region_hint: str = "") -> dict:
    h, w = frame.shape[:2]
    roi, roi_offset = frame, (0, 0)
    if region_hint:
        hint = region_hint.lower()
        if   "top"    in hint and "left"  in hint: roi = frame[:h//2, :w//2]
        elif "top"    in hint and "right" in hint: roi, roi_offset = frame[:h//2, w//2:], (w//2, 0)
        elif "bottom" in hint and "left"  in hint: roi, roi_offset = frame[h//2:, :w//2], (0, h//2)
        elif "bottom" in hint and "right" in hint: roi, roi_offset = frame[h//2:, w//2:], (w//2, h//2)
        elif "top"    in hint: roi = frame[:h//2, :]
        elif "bottom" in hint: roi, roi_offset = frame[h//2:, :], (0, h//2)
        elif "left"   in hint: roi = frame[:, :w//2]
        elif "right"  in hint: roi, roi_offset = frame[:, w//2:], (w//2, 0)

    hsv = cv2.GaussianBlur(cv2.cvtColor(roi, cv2.COLOR_BGR2HSV), (5, 5), 0)
    detections, seen = [], []
    for color_name, lower, upper in _LED_COLORS:
        kernel = cv2.getStructuringElement(cv2.MORPH_ELLIPSE, (3, 3))
        mask = cv2.morphologyEx(cv2.inRange(hsv, lower, upper), cv2.MORPH_OPEN, kernel)
        for cnt in cv2.findContours(mask, cv2.RETR_EXTERNAL, cv2.CHAIN_APPROX_SIMPLE)[0]:
            area = cv2.contourArea(cnt)
            if not (_MIN_AREA <= area <= _MAX_AREA):
                continue
            M = cv2.moments(cnt)
            if M["m00"] == 0:
                continue
            cx = int(M["m10"] / M["m00"]) + roi_offset[0]
            cy = int(M["m01"] / M["m00"]) + roi_offset[1]
            if any(abs(cx - px) < 15 and abs(cy - py) < 15 for px, py in seen):
                continue
            seen.append((cx, cy))
            mean_val = cv2.mean(hsv, mask=mask)
            conf = round(0.5 * mean_val[1] / 255.0 + 0.5 * min(area / 100.0, 1.0), 2)
            pos_x = "left" if cx < w // 3 else ("center" if cx < 2 * w // 3 else "right")
            pos_y = "top"  if cy < h // 3 else ("middle"  if cy < 2 * h // 3 else "bottom")
            detections.append({"color": color_name, "state": "on", "x": cx, "y": cy,
                                "area": int(area), "position": f"{pos_y}-{pos_x}", "confidence": conf})

    overall = sum(d["confidence"] for d in detections) / len(detections) if detections else 0.0
    return {"leds": detections, "confidence": round(overall, 2), "method": "opencv"}


def _format_opencv_result(result: dict) -> str:
    leds = result["leds"]
    if not leds:
        return "OpenCV: No LEDs detected."
    lines = [f"OpenCV detected {len(leds)} LED(s) [confidence: {result['confidence']:.0%}]:"]
    for i, led in enumerate(leds, 1):
        lines.append(f"  {i}. {led['color'].upper()} — ON — {led['position']} "
                     f"(px:{led['x']},{led['y']} area:{led['area']}px² conf:{led['confidence']:.0%})")
    return "\n".join(lines)


# ── AVFoundation focus control (best-effort) ──────────────────────────────────

def _set_focus_avfoundation(mode: str) -> str:
    """Set focus mode via AVFoundation. Requires pyobjc-framework-AVFoundation."""
    try:
        import AVFoundation as AVF
        devices = AVF.AVCaptureDevice.devicesWithMediaType_(AVF.AVMediaTypeVideo)
        mx = next((d for d in devices if d.uniqueID() == f"0x{1 << 28 | 0x046d0944:x}"
                   or "Brio" in d.localizedName()), None)
        if mx is None:
            return "AVFoundation: MX Brio not found."

        # AVCaptureFocusMode: 0=locked, 2=continuousAutoFocus
        focus_mode = 2 if mode == "auto" else 0
        if not mx.isFocusModeSupported_(focus_mode):
            return f"AVFoundation: focus mode '{mode}' not supported."

        err = mx.lockForConfiguration_(None)
        if err[1]:
            return f"AVFoundation: cannot lock device — {err[1]}"
        mx.setFocusMode_(focus_mode)
        mx.unlockForConfiguration()
        return f"Focus set to '{mode}' via AVFoundation."
    except Exception as e:
        return f"AVFoundation focus control failed: {e}"


# ── MCP tools ─────────────────────────────────────────────────────────────────

@mcp.tool()
def list_cameras() -> str:
    """Enumerate available camera devices. Returns index and resolution for each."""
    found = []
    for idx in range(8):
        cap = cv2.VideoCapture(idx)
        if cap.isOpened():
            w = int(cap.get(cv2.CAP_PROP_FRAME_WIDTH))
            h = int(cap.get(cv2.CAP_PROP_FRAME_HEIGHT))
            cap.release()
            found.append(f"[{idx}] {w}x{h}")
    return "\n".join(found) if found else "No cameras detected."


@mcp.tool()
def get_camera_info() -> str:
    """Return current camera settings: resolution, PTZ state, and image adjustments."""
    s = _state
    return (
        f"Camera index : {s['camera_index']}\n"
        f"Resolution   : {s['width']}x{s['height']}\n"
        f"Pan          : {s['pan']:+.2f}  (-1=full left, +1=full right)\n"
        f"Tilt         : {s['tilt']:+.2f}  (-1=full up,   +1=full down)\n"
        f"Zoom         : {s['zoom']:.2f}x\n"
        f"Brightness   : {s['brightness']:+d}  (-100…+100)\n"
        f"Contrast     : {s['contrast']:.2f}  (1.0=normal)\n"
        f"Saturation   : {s['saturation']:.2f}  (1.0=normal)\n"
        f"Sharpness    : {s['sharpness']}  (0…10)"
    )


@mcp.tool()
def set_resolution(width: int = 1920, height: int = 1080) -> str:
    """
    Switch capture resolution. Changes take effect on the next capture.

    Supported resolutions include: 1920x1080, 1280x720, 3840x2160 (4K),
    848x480, 640x480, 320x240 and more.

    Args:
        width: Frame width in pixels.
        height: Frame height in pixels.
    """
    cap = cv2.VideoCapture(_state["camera_index"])
    if not cap.isOpened():
        return "ERROR: Cannot open camera."
    cap.set(cv2.CAP_PROP_FRAME_WIDTH, width)
    cap.set(cv2.CAP_PROP_FRAME_HEIGHT, height)
    actual_w = int(cap.get(cv2.CAP_PROP_FRAME_WIDTH))
    actual_h = int(cap.get(cv2.CAP_PROP_FRAME_HEIGHT))
    cap.release()
    _state["width"] = actual_w
    _state["height"] = actual_h
    if (actual_w, actual_h) == (width, height):
        return f"Resolution set to {actual_w}x{actual_h}."
    return f"Requested {width}x{height}, camera snapped to nearest: {actual_w}x{actual_h}."


@mcp.tool()
def set_ptz(pan: float = 0.0, tilt: float = 0.0, zoom: float = 1.0) -> str:
    """
    Set software pan/tilt/zoom applied to every captured frame.

    Pan/tilt work by cropping the sensor region; zoom crops and upscales.
    Values are persistent until changed.

    Args:
        pan:  Horizontal position. -1.0=full left, 0.0=center, +1.0=full right.
        tilt: Vertical position.   -1.0=full up,   0.0=center, +1.0=full down.
        zoom: Zoom factor. 1.0=no zoom, 2.0=2× crop, max 4.0.
    """
    pan  = max(-1.0, min(1.0, pan))
    tilt = max(-1.0, min(1.0, tilt))
    zoom = max(1.0,  min(4.0, zoom))
    _state["pan"]  = pan
    _state["tilt"] = tilt
    _state["zoom"] = zoom
    return f"PTZ set — pan:{pan:+.2f}  tilt:{tilt:+.2f}  zoom:{zoom:.2f}x"


@mcp.tool()
def adjust_image(
    brightness: int   = 0,
    contrast:   float = 1.0,
    saturation: float = 1.0,
    sharpness:  int   = 0,
) -> str:
    """
    Set post-capture image adjustments applied to every frame.
    Values are persistent until changed. Pass only what you want to change.

    Args:
        brightness: Additive offset. -100 (darker) … +100 (brighter). Default 0.
        contrast:   Multiplicative scale. 0.5=low, 1.0=normal, 2.0=high. Default 1.0.
        saturation: Color intensity. 0.0=grayscale, 1.0=normal, 2.0=vivid. Default 1.0.
        sharpness:  Unsharp mask strength. 0=off, 10=maximum. Default 0.
    """
    _state["brightness"] = max(-100, min(100, brightness))
    _state["contrast"]   = max(0.0,  min(3.0, contrast))
    _state["saturation"] = max(0.0,  min(3.0, saturation))
    _state["sharpness"]  = max(0,    min(10,  sharpness))
    return (f"Image adjustments set — "
            f"brightness:{_state['brightness']:+d}  "
            f"contrast:{_state['contrast']:.2f}  "
            f"saturation:{_state['saturation']:.2f}  "
            f"sharpness:{_state['sharpness']}")


@mcp.tool()
def set_focus(mode: str = "auto") -> str:
    """
    Set camera focus mode via AVFoundation.

    Args:
        mode: 'auto' for continuous autofocus, 'manual' to lock current focus position.
    """
    if mode not in ("auto", "manual"):
        return "ERROR: mode must be 'auto' or 'manual'."
    return _set_focus_avfoundation(mode)


@mcp.tool()
def capture_frame(camera_index: int = -1, width: int = -1, height: int = -1) -> str:
    """
    Capture a still frame and return it as a base64-encoded JPEG.
    Applies current PTZ and image adjustments.

    Args:
        camera_index: Camera index. -1 = use current setting.
        width:  Capture width.  -1 = use current setting.
        height: Capture height. -1 = use current setting.
    """
    if camera_index >= 0: _state["camera_index"] = camera_index
    if width  > 0:        _state["width"]  = width
    if height > 0:        _state["height"] = height
    try:
        frame = _get_processed_frame()
        jpeg  = _bgr_to_jpeg(frame)
        b64   = _frame_to_base64(jpeg)
        return f"FRAME_BASE64:{b64}\nSIZE:{len(jpeg)//1024}KB  RES:{frame.shape[1]}x{frame.shape[0]}"
    except Exception as e:
        return f"ERROR: {e}"


@mcp.tool()
def analyze_frame(
    prompt: str = "Describe what you see in this image.",
    camera_index: int = -1,
) -> str:
    """
    Capture a frame and send it to Claude vision for analysis.
    Applies current PTZ and image adjustments. Requires ANTHROPIC_API_KEY.

    Args:
        prompt: Question or instruction for the vision model.
        camera_index: Camera index. -1 = use current setting.
    """
    if not _api_available():
        return "ERROR: ANTHROPIC_API_KEY not set."
    if camera_index >= 0:
        _state["camera_index"] = camera_index
    try:
        frame = _get_processed_frame()
        jpeg  = _bgr_to_jpeg(frame)
        b64   = _frame_to_base64(jpeg)
        msg   = _get_client().messages.create(
            model="claude-opus-4-6",
            max_tokens=1024,
            messages=[{"role": "user", "content": [
                {"type": "image", "source": {"type": "base64", "media_type": "image/jpeg", "data": b64}},
                {"type": "text",  "text": prompt},
            ]}],
        )
        return msg.content[0].text
    except Exception as e:
        return f"ERROR: {e}"


@mcp.tool()
def detect_led_state(camera_index: int = -1, region_hint: str = "") -> str:
    """
    Detect LED states on a hardware board.
    Uses OpenCV HSV detection first; falls back to Claude vision if confidence < 60%.

    Args:
        camera_index: Camera index. -1 = use current setting.
        region_hint:  Optional crop hint, e.g. 'top-left', 'bottom-right'.
    """
    if camera_index >= 0:
        _state["camera_index"] = camera_index
    try:
        frame = _get_processed_frame()
    except Exception as e:
        return f"ERROR: {e}"

    result      = _detect_leds_opencv(frame, region_hint)
    opencv_text = _format_opencv_result(result)

    if result["confidence"] >= _CONFIDENCE_THRESHOLD or not _api_available():
        note = "" if result["confidence"] >= _CONFIDENCE_THRESHOLD else \
            "\n[Low confidence — ANTHROPIC_API_KEY not set, OpenCV result only]"
        return opencv_text + note

    hint   = f" Focus on the {region_hint}." if region_hint else ""
    prompt = ("Inspect this embedded hardware board. "
              "For each visible LED report: color, on/off, approximate position. "
              "If no LEDs are visible, say so." + hint)
    try:
        jpeg = _bgr_to_jpeg(frame)
        b64  = _frame_to_base64(jpeg)
        msg  = _get_client().messages.create(
            model="claude-opus-4-6", max_tokens=512,
            messages=[{"role": "user", "content": [
                {"type": "image", "source": {"type": "base64", "media_type": "image/jpeg", "data": b64}},
                {"type": "text",  "text": prompt},
            ]}],
        )
        return (f"{opencv_text}\n"
                f"[Low confidence ({result['confidence']:.0%}) — Claude vision fallback:]\n"
                f"{msg.content[0].text}")
    except Exception as e:
        return f"{opencv_text}\n[Fallback failed: {e}]"


# ── Board debugging tools ─────────────────────────────────────────────────────

@mcp.tool()
def read_display(
    region_hint: str = "",
    camera_index: int = -1,
) -> str:
    """
    Read text from an LCD, OLED, or e-ink display on the board using OCR.
    Uses Tesseract OCR with adaptive preprocessing. Falls back to Claude vision
    if OCR yields no text and ANTHROPIC_API_KEY is set.

    Args:
        region_hint: Crop hint to isolate the display area, e.g. 'top-left', 'bottom'.
        camera_index: Camera index. -1 = use current setting.
    """
    if camera_index >= 0:
        _state["camera_index"] = camera_index
    try:
        frame = _get_processed_frame()
    except Exception as e:
        return f"ERROR: {e}"

    try:
        import pytesseract
    except ImportError:
        return "ERROR: pytesseract not installed. Run: pip install pytesseract"

    h, w = frame.shape[:2]
    roi = frame
    if region_hint:
        hint = region_hint.lower()
        if   "top"    in hint and "left"  in hint: roi = frame[:h//2, :w//2]
        elif "top"    in hint and "right" in hint: roi = frame[:h//2, w//2:]
        elif "bottom" in hint and "left"  in hint: roi = frame[h//2:, :w//2]
        elif "bottom" in hint and "right" in hint: roi = frame[h//2:, w//2:]
        elif "top"    in hint: roi = frame[:h//2, :]
        elif "bottom" in hint: roi = frame[h//2:, :]
        elif "left"   in hint: roi = frame[:, :w//2]
        elif "right"  in hint: roi = frame[:, w//2:]

    # Upscale small ROIs for better OCR accuracy
    rh, rw = roi.shape[:2]
    if rw < 640:
        scale = 640 / rw
        roi = cv2.resize(roi, (int(rw * scale), int(rh * scale)), interpolation=cv2.INTER_CUBIC)

    gray = cv2.cvtColor(roi, cv2.COLOR_BGR2GRAY)

    # Try multiple preprocessing strategies, pick the one with most text
    results = []
    configs = [
        ("adaptive_thresh", cv2.adaptiveThreshold(gray, 255, cv2.ADAPTIVE_THRESH_GAUSSIAN_C,
                                                   cv2.THRESH_BINARY, 11, 2)),
        ("otsu",            cv2.threshold(gray, 0, 255, cv2.THRESH_BINARY + cv2.THRESH_OTSU)[1]),
        ("inverted_otsu",   cv2.threshold(gray, 0, 255, cv2.THRESH_BINARY_INV + cv2.THRESH_OTSU)[1]),
    ]
    for label, processed in configs:
        text = pytesseract.image_to_string(processed, config="--psm 6").strip()
        if text:
            results.append((label, text))

    if results:
        # Return the longest result (most text extracted)
        best_label, best_text = max(results, key=lambda x: len(x[1]))
        return f"OCR ({best_label}):\n{best_text}"

    # Fallback to Claude vision
    if not _api_available():
        return "OCR: No text detected. ANTHROPIC_API_KEY not set — cannot use vision fallback."
    try:
        jpeg = _bgr_to_jpeg(frame)
        b64  = _frame_to_base64(jpeg)
        hint_str = f" Focus on the {region_hint}." if region_hint else ""
        prompt = ("Read all text visible on any display (LCD, OLED, e-ink, 7-segment) "
                  "on this hardware board. Return the exact text shown." + hint_str)
        msg = _get_client().messages.create(
            model="claude-opus-4-6", max_tokens=512,
            messages=[{"role": "user", "content": [
                {"type": "image", "source": {"type": "base64", "media_type": "image/jpeg", "data": b64}},
                {"type": "text",  "text": prompt},
            ]}],
        )
        return f"OCR: No text detected.\n[Claude vision fallback:]\n{msg.content[0].text}"
    except Exception as e:
        return f"OCR: No text detected. Fallback failed: {e}"


@mcp.tool()
def detect_jumper(
    region_hint: str = "",
    expected: str = "present",
    camera_index: int = -1,
) -> str:
    """
    Detect whether a jumper (shunt) is installed on a header pin.
    Uses blob detection to find small rectangular plastic objects in the region.

    Args:
        region_hint: Where to look, e.g. 'top-left', 'center'. Defaults to full frame.
        expected:    'present' or 'absent' — what you expect. Used to flag mismatches.
        camera_index: Camera index. -1 = use current setting.
    """
    if camera_index >= 0:
        _state["camera_index"] = camera_index
    try:
        frame = _get_processed_frame()
    except Exception as e:
        return f"ERROR: {e}"

    h, w = frame.shape[:2]
    roi, roi_offset = frame, (0, 0)
    if region_hint:
        hint = region_hint.lower()
        if   "top"    in hint and "left"  in hint: roi = frame[:h//2, :w//2]
        elif "top"    in hint and "right" in hint: roi, roi_offset = frame[:h//2, w//2:], (w//2, 0)
        elif "bottom" in hint and "left"  in hint: roi, roi_offset = frame[h//2:, :w//2], (0, h//2)
        elif "bottom" in hint and "right" in hint: roi, roi_offset = frame[h//2:, w//2:], (w//2, h//2)
        elif "top"    in hint: roi = frame[:h//2, :]
        elif "bottom" in hint: roi, roi_offset = frame[h//2:, :], (0, h//2)
        elif "left"   in hint: roi = frame[:, :w//2]
        elif "right"  in hint: roi, roi_offset = frame[:, w//2:], (w//2, 0)

    # Jumpers are small, opaque, roughly rectangular plastic blobs
    # Common colors: black, red, yellow, green, blue
    # Strategy: find compact, roughly rectangular contours in typical jumper size range

    gray    = cv2.cvtColor(roi, cv2.COLOR_BGR2GRAY)
    blurred = cv2.GaussianBlur(gray, (5, 5), 0)
    edges   = cv2.Canny(blurred, 30, 100)
    kernel  = cv2.getStructuringElement(cv2.MORPH_RECT, (3, 3))
    closed  = cv2.morphologyEx(edges, cv2.MORPH_CLOSE, kernel)

    rh, rw  = roi.shape[:2]
    # Jumper size heuristic: 0.3%–3% of ROI area
    min_area = rh * rw * 0.003
    max_area = rh * rw * 0.03

    candidates = []
    for cnt in cv2.findContours(closed, cv2.RETR_EXTERNAL, cv2.CHAIN_APPROX_SIMPLE)[0]:
        area = cv2.contourArea(cnt)
        if not (min_area <= area <= max_area):
            continue
        x, y, bw, bh = cv2.boundingRect(cnt)
        aspect = max(bw, bh) / max(min(bw, bh), 1)
        # Jumpers are roughly square to 2:1 aspect
        if aspect > 3.5:
            continue
        rect_fill = area / (bw * bh + 1e-6)
        if rect_fill < 0.4:
            continue
        cx = x + bw // 2 + roi_offset[0]
        cy = y + bh // 2 + roi_offset[1]
        pos_x = "left" if cx < w // 3 else ("center" if cx < 2 * w // 3 else "right")
        pos_y = "top"  if cy < h // 3 else ("middle"  if cy < 2 * h // 3 else "bottom")
        # Sample dominant color of the blob
        mask = np.zeros(roi.shape[:2], dtype=np.uint8)
        cv2.drawContours(mask, [cnt], -1, 255, -1)
        mean_bgr = cv2.mean(roi, mask=mask)[:3]
        candidates.append({
            "position": f"{pos_y}-{pos_x}",
            "area": int(area),
            "color_bgr": tuple(int(v) for v in mean_bgr),
        })

    detected = len(candidates) > 0
    status   = "PRESENT" if detected else "ABSENT"
    mismatch = (expected == "present" and not detected) or (expected == "absent" and detected)
    flag     = " ⚠️  MISMATCH — check before proceeding" if mismatch else ""

    lines = [f"Jumper: {status}{flag}"]
    for i, c in enumerate(candidates, 1):
        b, g, r = c["color_bgr"]
        lines.append(f"  {i}. position={c['position']}  area={c['area']}px²  "
                     f"color≈RGB({r},{g},{b})")
    if not candidates:
        lines.append("  No jumper-like objects found in the search region.")
    return "\n".join(lines)


@mcp.tool()
def check_board(camera_index: int = -1) -> str:
    """
    Detect whether a PCB is present in the camera frame and estimate its orientation.
    Uses PCB-green color detection and largest-contour angle analysis.
    No reference image required.

    Args:
        camera_index: Camera index. -1 = use current setting.
    """
    if camera_index >= 0:
        _state["camera_index"] = camera_index
    try:
        frame = _get_processed_frame()
    except Exception as e:
        return f"ERROR: {e}"

    h, w = frame.shape[:2]
    hsv  = cv2.cvtColor(frame, cv2.COLOR_BGR2HSV)

    # PCB green / FR4 green color range in HSV
    pcb_lower = np.array([35,  30,  30])
    pcb_upper = np.array([90, 255, 200])
    mask = cv2.inRange(hsv, pcb_lower, pcb_upper)
    kernel = cv2.getStructuringElement(cv2.MORPH_RECT, (15, 15))
    mask   = cv2.morphologyEx(mask, cv2.MORPH_CLOSE, kernel)
    mask   = cv2.morphologyEx(mask, cv2.MORPH_OPEN,  kernel)

    pcb_pixel_fraction = np.count_nonzero(mask) / (h * w)

    if pcb_pixel_fraction < 0.02:
        return ("Board: NOT DETECTED\n"
                f"PCB-green coverage: {pcb_pixel_fraction:.1%} (threshold: 2%)\n"
                "Ensure the board is in frame and well-lit.")

    # Find largest contour to estimate orientation
    contours, _ = cv2.findContours(mask, cv2.RETR_EXTERNAL, cv2.CHAIN_APPROX_SIMPLE)
    if not contours:
        return f"Board: PRESENT (coverage {pcb_pixel_fraction:.1%}) — orientation unknown"

    largest = max(contours, key=cv2.contourArea)
    rect    = cv2.minAreaRect(largest)
    angle   = rect[2]  # degrees, -90 to 0
    # Normalize to 0–90° tilt from horizontal
    if angle < -45:
        angle += 90
    tilt = abs(angle)

    orientation = "aligned (< 5°)" if tilt < 5 else \
                  f"slightly tilted ({tilt:.1f}°)" if tilt < 15 else \
                  f"tilted ({tilt:.1f}°) — reposition for best LED detection"

    bx, by, bw_box, bh_box = cv2.boundingRect(largest)
    center_x = bx + bw_box // 2
    center_y = by + bh_box // 2
    pos_x = "left" if center_x < w // 3 else ("center" if center_x < 2 * w // 3 else "right")
    pos_y = "top"  if center_y < h // 3 else ("middle"  if center_y < 2 * h // 3 else "bottom")

    return (f"Board: PRESENT\n"
            f"  PCB-green coverage : {pcb_pixel_fraction:.1%}\n"
            f"  Bounding box       : {bw_box}×{bh_box}px at {pos_y}-{pos_x}\n"
            f"  Orientation        : {orientation}")


@mcp.tool()
def detect_motion(
    duration_s: float = 3.0,
    sensitivity: int = 20,
    camera_index: int = -1,
) -> str:
    """
    Monitor the camera for motion over a time window.
    Useful for detecting board resets (brief flicker), relay actuation,
    or unexpected physical disturbances.

    Args:
        duration_s:  How long to monitor in seconds. Default 3.0.
        sensitivity: Pixel difference threshold (1–100). Lower = more sensitive. Default 20.
        camera_index: Camera index. -1 = use current setting.
    """
    import time

    if camera_index >= 0:
        _state["camera_index"] = camera_index

    cap = cv2.VideoCapture(_state["camera_index"])
    if not cap.isOpened():
        return f"ERROR: Cannot open camera index {_state['camera_index']}"
    cap.set(cv2.CAP_PROP_FRAME_WIDTH,  _state["width"])
    cap.set(cv2.CAP_PROP_FRAME_HEIGHT, _state["height"])
    # Warm up
    for _ in range(3):
        cap.read()

    frames_captured = 0
    motion_events   = []
    peak_diff       = 0.0
    prev_gray       = None
    deadline        = time.time() + duration_s

    while time.time() < deadline:
        ret, frame = cap.read()
        if not ret:
            break
        frames_captured += 1
        gray = cv2.cvtColor(frame, cv2.COLOR_BGR2GRAY)
        gray = cv2.GaussianBlur(gray, (21, 21), 0)

        if prev_gray is not None:
            diff  = cv2.absdiff(prev_gray, gray)
            thresh = cv2.threshold(diff, sensitivity, 255, cv2.THRESH_BINARY)[1]
            changed_fraction = np.count_nonzero(thresh) / thresh.size
            peak_diff = max(peak_diff, changed_fraction)

            if changed_fraction > 0.01:   # >1% of frame changed
                elapsed = duration_s - (deadline - time.time())
                motion_events.append({
                    "t_s": round(elapsed, 2),
                    "fraction": round(changed_fraction, 3),
                    "magnitude": "large" if changed_fraction > 0.1 else
                                 "medium" if changed_fraction > 0.03 else "small",
                })
        prev_gray = gray

    cap.release()

    lines = [
        f"Motion monitoring: {duration_s:.1f}s  frames={frames_captured}  "
        f"sensitivity={sensitivity}  peak_diff={peak_diff:.1%}"
    ]

    if not motion_events:
        lines.append("Result: NO MOTION DETECTED — scene was stable.")
    else:
        reset_likely = any(e["magnitude"] == "large" for e in motion_events)
        lines.append(f"Result: {len(motion_events)} motion event(s) detected"
                     + (" — possible board RESET (large frame change)" if reset_likely else ""))
        for e in motion_events:
            lines.append(f"  t={e['t_s']}s  changed={e['fraction']:.1%}  [{e['magnitude']}]")

    return "\n".join(lines)


@mcp.tool()
def read_qr_code(camera_index: int = -1) -> str:
    """
    Detect and decode QR codes and barcodes in the camera frame.
    Useful for reading board serial numbers, hardware revisions, or firmware labels.
    Uses OpenCV's built-in QR detector (no external library required).

    Args:
        camera_index: Camera index. -1 = use current setting.
    """
    if camera_index >= 0:
        _state["camera_index"] = camera_index
    try:
        frame = _get_processed_frame()
    except Exception as e:
        return f"ERROR: {e}"

    gray = cv2.cvtColor(frame, cv2.COLOR_BGR2GRAY)

    results = []

    # ── Standard QR detector ─────────────────────────────────────────────────
    qr = cv2.QRCodeDetector()
    data, points, _ = qr.detectAndDecode(gray)
    if data:
        results.append(("QR", data))

    # ── Multi-QR detector (OpenCV 4.5+) ──────────────────────────────────────
    try:
        mqr   = cv2.QRCodeDetectorAruco()
        texts, pts, _ = mqr.detectAndDecodeMulti(gray)
        for t in (texts or []):
            if t and t != data:
                results.append(("QR", t))
    except Exception:
        pass

    # ── WeChatQR (better detection of small/damaged codes, OpenCV contrib) ───
    try:
        wechat = cv2.wechat_qrcode_WeChatQRCode()
        texts, _ = wechat.detectAndDecode(gray)
        for t in (texts or []):
            if t and t not in [r[1] for r in results]:
                results.append(("WeChatQR", t))
    except Exception:
        pass

    if not results:
        # Try with preprocessed image (higher contrast)
        _, binary = cv2.threshold(gray, 0, 255, cv2.THRESH_BINARY + cv2.THRESH_OTSU)
        data2, _, _ = qr.detectAndDecode(binary)
        if data2:
            results.append(("QR (enhanced)", data2))

    if not results:
        return ("No QR code or barcode detected.\n"
                "Tips: ensure good lighting, code is fully in frame, "
                "try set_ptz(zoom=2.0) to enlarge the code.")

    lines = [f"Detected {len(results)} code(s):"]
    for i, (kind, text) in enumerate(results, 1):
        lines.append(f"  {i}. [{kind}] {text}")
    return "\n".join(lines)


if __name__ == "__main__":
    mcp.run()
