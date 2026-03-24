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


if __name__ == "__main__":
    mcp.run()
