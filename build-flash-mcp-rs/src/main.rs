use anyhow::Result;
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::Deserialize;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::Arc,
};

// ── device registry ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Default)]
struct BoardConfig {
    description: Option<String>,
    // build
    project_path: Option<String>,
    preset: Option<String>,
    build_tool: Option<String>,   // "cmake" (default) | "idf"
    idf_path: Option<String>,    // path to ESP-IDF root (for build_tool="idf")
    // flash
    flash_tool: Option<String>,   // "openocd" | "esptool" | "idf" | "probe-rs"
    flash_port: Option<String>,   // esptool/idf: serial port
    flash_baud: Option<u32>,      // esptool/idf: baud rate (default 921600)
    openocd_cfg: Option<String>,  // openocd: path relative to scripts dir
    // target — for probe-rs flash + arch-aware size tool
    target: Option<String>,
    // probe_serial — for probe-rs
    probe_serial: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct DeviceRegistry {
    #[serde(default)]
    board: HashMap<String, BoardConfig>,
}

fn load_registry() -> DeviceRegistry {
    let path = std::env::var("BUILD_FLASH_MCP_CONFIG").unwrap_or_default();
    if path.is_empty() {
        return DeviceRegistry::default();
    }
    match std::fs::read_to_string(&path) {
        Ok(content) => toml::from_str::<DeviceRegistry>(&content).unwrap_or_else(|e| {
            eprintln!("build-flash-mcp: cannot parse {}: {}", path, e);
            DeviceRegistry::default()
        }),
        Err(e) => {
            eprintln!("build-flash-mcp: cannot read {}: {}", path, e);
            DeviceRegistry::default()
        }
    }
}

#[derive(Clone)]
struct ResolvedProject {
    project_path: String,
    preset: String,
    build_tool: String,   // "cmake" | "idf"
    idf_path: String,     // ESP-IDF root for build_tool="idf"
    flash_tool: String,
    flash_port: String,
    flash_baud: u32,
    openocd_cfg: String,
    target: String,
    probe_serial: Option<String>,
    label: String,
}

fn resolve_project(
    board: Option<&str>,
    explicit_path: Option<&str>,
    explicit_preset: Option<&str>,
    registry: &DeviceRegistry,
) -> Result<ResolvedProject, String> {
    let mut proj = ResolvedProject {
        project_path: String::new(),
        preset: "Debug".to_string(),
        build_tool: "cmake".to_string(),
        idf_path: String::new(),
        flash_tool: "openocd".to_string(),
        flash_port: String::new(),
        flash_baud: 921600,
        openocd_cfg: String::new(),
        target: String::new(),
        probe_serial: None,
        label: "direct".to_string(),
    };

    if let Some(b) = board {
        let cfg = registry.board.get(b).ok_or_else(|| {
            let names: Vec<_> = registry.board.keys().cloned().collect();
            let avail = if names.is_empty() {
                "none (BUILD_FLASH_MCP_CONFIG not set or empty)".to_string()
            } else {
                names.join(", ")
            };
            format!("Board '{}' not found. Available: {}", b, avail)
        })?;
        proj.label = b.to_string();
        if let Some(v) = &cfg.project_path { proj.project_path = v.clone(); }
        if let Some(v) = &cfg.preset { proj.preset = v.clone(); }
        if let Some(v) = &cfg.build_tool { proj.build_tool = v.clone(); }
        if let Some(v) = &cfg.idf_path { proj.idf_path = v.clone(); }
        if let Some(v) = &cfg.flash_tool { proj.flash_tool = v.clone(); }
        if let Some(v) = &cfg.flash_port { proj.flash_port = v.clone(); }
        if let Some(v) = cfg.flash_baud { proj.flash_baud = v; }
        if let Some(v) = &cfg.openocd_cfg { proj.openocd_cfg = v.clone(); }
        if let Some(v) = &cfg.target { proj.target = v.clone(); }
        if let Some(v) = &cfg.probe_serial { proj.probe_serial = Some(v.clone()); }
    }

    // Explicit params override registry values
    if let Some(v) = explicit_path { if !v.is_empty() { proj.project_path = v.to_string(); } }
    if let Some(v) = explicit_preset { if !v.is_empty() { proj.preset = v.to_string(); } }

    if proj.project_path.is_empty() {
        return Err("No project_path. Pass project_path directly or configure a board in devices.toml.".to_string());
    }

    Ok(proj)
}

// ── run helpers ───────────────────────────────────────────────────────────────

struct RunResult {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run(args: &[&str], cwd: Option<&Path>) -> Result<RunResult> {
    let mut cmd = Command::new(args[0]);
    cmd.args(&args[1..]);
    if let Some(dir) = cwd { cmd.current_dir(dir); }
    let output: Output = cmd.output()?;
    Ok(RunResult {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn extract_errors(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|l| l.to_lowercase().contains("error:"))
        .map(|l| l.trim())
        .collect()
}

fn find_elfs(root: &Path) -> Vec<PathBuf> {
    let pattern = format!("{}/**/*.elf", root.display());
    let mut elfs: Vec<PathBuf> = glob::glob(&pattern)
        .into_iter().flatten().flatten().collect();
    elfs.sort();
    elfs
}

fn summarise_failure(out: &str, err: &str) -> String {
    let errors: Vec<&str> = extract_errors(err).into_iter().chain(extract_errors(out)).collect();
    if !errors.is_empty() {
        errors[..errors.len().min(15)].join("\n")
    } else {
        err.lines().rev().take(10).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")
    }
}

// ── arch-aware size tool ──────────────────────────────────────────────────────

fn size_binary(target: &str) -> &'static str {
    let t = target.to_lowercase();
    if t.starts_with("esp32s3") || t.starts_with("esp32s2") || t.contains("xtensa-esp32s3") {
        "xtensa-esp32s3-elf-size"
    } else if t.starts_with("esp32c") || t.contains("riscv32") {
        "riscv32-esp-elf-size"
    } else if t.starts_with("esp32") {
        "xtensa-lx106-elf-size"
    } else {
        "arm-none-eabi-size"
    }
}

fn run_size(elf: &Path, target: &str) -> String {
    let size_bin = size_binary(target);
    if let Ok(r) = run(&[size_bin, &elf.to_string_lossy()], None) {
        if r.code == 0 { return r.stdout.trim().to_string(); }
    }
    if let Ok(r) = run(&["size", &elf.to_string_lossy()], None) {
        if r.code == 0 { return r.stdout.trim().to_string(); }
    }
    format!("(size binary '{}' not available)", size_bin)
}

// ── OpenOCD scripts path ──────────────────────────────────────────────────────

fn find_openocd_scripts() -> String {
    if let Ok(r) = run(&["brew", "--prefix", "open-ocd"], None) {
        if r.code == 0 {
            let scripts = format!("{}/share/openocd/scripts", r.stdout.trim());
            if Path::new(&scripts).exists() { return scripts; }
        }
    }
    for p in &[
        "/opt/homebrew/share/openocd/scripts",
        "/usr/local/share/openocd/scripts",
        "/usr/share/openocd/scripts",
    ] {
        if Path::new(p).exists() { return p.to_string(); }
    }
    "/usr/share/openocd/scripts".to_string()
}

// ── tool implementations ──────────────────────────────────────────────────────

fn resolve_build_dir(proj: &ResolvedProject) -> PathBuf {
    let root = Path::new(&proj.project_path);
    if proj.preset.is_empty() {
        root.join("build")
    } else {
        root.join("build").join(&proj.preset)
    }
}

fn do_build_firmware(
    proj: &ResolvedProject,
    toolchain_file: &str,
    build_dir_override: &str,
    build_type: &str,
) -> Result<String> {
    let root = Path::new(&proj.project_path);

    if proj.build_tool == "idf" {
        // ESP-IDF: source export.sh to get full toolchain env, then run idf.py build
        if proj.idf_path.is_empty() {
            return Ok("ERROR: idf_path not configured. Set idf_path in devices.toml to the ESP-IDF root.".to_string());
        }
        let build_dir = resolve_build_dir(proj);
        let build_dir_str = build_dir.to_string_lossy().into_owned();
        let cmd = format!(
            "source {}/export.sh 2>/dev/null && idf.py -B {} build",
            proj.idf_path, build_dir_str
        );
        let r = run(&["bash", "-c", &cmd], Some(root))?;
        if r.code != 0 {
            return Ok(format!("Build FAILED (idf.py):\n{}", summarise_failure(&r.stdout, &r.stderr)));
        }
    } else if !proj.preset.is_empty() {
        let presets_file = root.join("CMakePresets.json");
        if !presets_file.exists() {
            return Ok(format!("ERROR: CMakePresets.json not found in {}", proj.project_path));
        }
        let build_path = root.join("build").join(&proj.preset);
        if !build_path.join("CMakeCache.txt").exists() {
            let r = run(&["cmake", "--preset", &proj.preset], Some(root))?;
            if r.code != 0 {
                return Ok(format!("Configure FAILED:\n{}", summarise_failure(&r.stdout, &r.stderr)));
            }
        }
        let r = run(&["cmake", "--build", "--preset", &proj.preset], Some(root))?;
        if r.code != 0 {
            return Ok(format!("Build FAILED:\n{}", summarise_failure(&r.stdout, &r.stderr)));
        }
    } else {
        if toolchain_file.is_empty() {
            return Ok("ERROR: Provide either a preset name, set build_tool='idf', or provide toolchain_file.".to_string());
        }
        let bp = root.join(build_dir_override);
        let bp_str = bp.to_string_lossy().into_owned();
        if !bp.join("CMakeCache.txt").exists() {
            let r = run(&[
                "cmake", "-B", &bp_str, "-G", "Ninja",
                &format!("-DCMAKE_TOOLCHAIN_FILE={}", toolchain_file),
                &format!("-DCMAKE_BUILD_TYPE={}", build_type),
                &proj.project_path,
            ], None)?;
            if r.code != 0 {
                return Ok(format!("Configure FAILED:\n{}", summarise_failure(&r.stdout, &r.stderr)));
            }
        }
        let r = run(&["cmake", "--build", &bp_str, "--", "-j4"], None)?;
        if r.code != 0 {
            return Ok(format!("Build FAILED:\n{}", summarise_failure(&r.stdout, &r.stderr)));
        }
    }

    let elfs = find_elfs(root);
    let mut size_info = String::new();
    if !elfs.is_empty() {
        let lines: Vec<String> = elfs.iter().map(|elf| {
            let name = elf.file_name().unwrap_or_default().to_string_lossy();
            format!("{}:\n{}", name, run_size(elf, &proj.target))
        }).collect();
        size_info = format!("\nBinary sizes:\n{}", lines.join("\n\n"));
    }

    Ok(format!("Build SUCCESS (board: {}, preset: {}).{}", proj.label, proj.preset, size_info))
}

fn do_clean_build(proj: &ResolvedProject, build_dir_override: &str) -> String {
    let build_path = if !proj.preset.is_empty() {
        Path::new(&proj.project_path).join("build").join(&proj.preset)
    } else {
        Path::new(&proj.project_path).join(build_dir_override)
    };
    if !build_path.exists() {
        return format!("Nothing to clean — {} does not exist.", build_path.display());
    }
    match std::fs::remove_dir_all(&build_path) {
        Ok(_) => format!("Cleaned {}. Next build will reconfigure.", build_path.display()),
        Err(e) => format!("ERROR: Could not remove {}: {}", build_path.display(), e),
    }
}

fn do_get_build_size(proj: &ResolvedProject) -> String {
    let root = Path::new(&proj.project_path);
    let elfs = find_elfs(root);
    if elfs.is_empty() {
        return format!("No .elf file found under {}. Run build_firmware first.", proj.project_path);
    }
    let lines: Vec<String> = elfs.iter().map(|elf| {
        let name = elf.file_name().unwrap_or_default().to_string_lossy();
        format!("{}:\n{}", name, run_size(elf, &proj.target))
    }).collect();
    let arch = if proj.target.is_empty() { "arm-none-eabi" } else { size_binary(&proj.target) };
    format!("Build sizes (board: {}, using {}):\n{}", proj.label, arch, lines.join("\n\n"))
}

fn do_flash_openocd(proj: &ResolvedProject) -> Result<String> {
    let root = Path::new(&proj.project_path);
    let elfs = find_elfs(root);
    if elfs.is_empty() {
        return Ok(format!("No .elf found under {}. Run build_firmware first.", proj.project_path));
    }
    if proj.openocd_cfg.is_empty() {
        return Ok("ERROR: openocd_cfg not configured. Set openocd_cfg in devices.toml (e.g. 'target/stm32wlx.cfg').".to_string());
    }

    let scripts_dir = find_openocd_scripts();
    let program_cmds: String = elfs.iter()
        .map(|e| format!("program {{{}}} verify;", e.display()))
        .collect::<Vec<_>>().join(" ");
    let openocd_cmd = format!("{} reset halt; resume; exit", program_cmds);

    let r = if Path::new(&proj.openocd_cfg).is_absolute() {
        run(&["openocd", "-f", "interface/stlink.cfg", "-f", &proj.openocd_cfg, "-c", &openocd_cmd], None)?
    } else {
        run(&["openocd", "-s", &scripts_dir, "-f", "interface/stlink.cfg", "-f", &proj.openocd_cfg, "-c", &openocd_cmd], None)?
    };

    let combined = format!("{}{}", r.stdout, r.stderr).trim().to_string();
    if r.code != 0 {
        let errors = extract_errors(&combined);
        let msg = if !errors.is_empty() {
            errors[..errors.len().min(10)].join("\n")
        } else {
            combined.lines().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")
        };
        return Ok(format!("Flash FAILED (openocd):\n{}", msg));
    }

    let flashed: Vec<String> = elfs.iter()
        .map(|e| e.file_name().unwrap_or_default().to_string_lossy().into_owned())
        .collect();
    if combined.to_lowercase().contains("verified") {
        Ok(format!("Flash SUCCESS (openocd). Verified and reset.\nFlashed: {}", flashed.join(", ")))
    } else {
        Ok(format!("Flash completed (openocd).\n{}", &combined[combined.len().saturating_sub(300)..]))
    }
}

fn do_flash_esptool(proj: &ResolvedProject) -> Result<String> {
    if proj.flash_port.is_empty() {
        return Ok("ERROR: flash_port not configured. Set flash_port in devices.toml or use flash_tool='idf'.".to_string());
    }
    let build_dir = resolve_build_dir(proj);
    let flasher_args_path = build_dir.join("flasher_args.json");
    if !flasher_args_path.exists() {
        return Ok(format!(
            "ERROR: {} not found. Run build_firmware first (ESP-IDF CMake generates this file).",
            flasher_args_path.display()
        ));
    }

    let content = std::fs::read_to_string(&flasher_args_path)?;
    let parsed: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| anyhow::anyhow!("Cannot parse flasher_args.json: {}", e))?;

    // Build command: esptool.py --port <port> --baud <baud> write_flash [write_flash_args] <addr file>...
    let mut args: Vec<String> = vec![
        "esptool.py".to_string(),
        "--port".to_string(), proj.flash_port.clone(),
        "--baud".to_string(), proj.flash_baud.to_string(),
        "write_flash".to_string(),
    ];
    if let Some(wf_args) = parsed.get("write_flash_args").and_then(|v| v.as_array()) {
        for a in wf_args {
            if let Some(s) = a.as_str() { args.push(s.to_string()); }
        }
    }

    let flash_files = parsed.get("flash_files").and_then(|v| v.as_object())
        .ok_or_else(|| anyhow::anyhow!("No flash_files in flasher_args.json"))?;

    let mut file_args: Vec<(u64, String)> = flash_files.iter()
        .filter_map(|(addr_str, file_val)| {
            let addr = u64::from_str_radix(addr_str.trim_start_matches("0x"), 16).ok()?;
            let file = file_val.as_str()?;
            let full = build_dir.join(file).to_string_lossy().into_owned();
            Some((addr, full))
        })
        .collect();
    file_args.sort_by_key(|(addr, _)| *addr);
    for (addr, file) in &file_args {
        args.push(format!("0x{:x}", addr));
        args.push(format!("\"{}\"", file)); // quote paths for bash invocation
    }

    // If idf_path is set, source export.sh so esptool.py is in PATH
    let r = if !proj.idf_path.is_empty() {
        let cmd = format!(
            "source {}/export.sh 2>/dev/null && {}",
            proj.idf_path,
            args.join(" ")
        );
        run(&["bash", "-c", &cmd], None)?
    } else {
        // Try esptool.py directly (must be in PATH)
        let args_unquoted: Vec<String> = {
            // rebuild without quotes for direct exec
            let mut a: Vec<String> = vec![
                "esptool.py".to_string(),
                "--port".to_string(), proj.flash_port.clone(),
                "--baud".to_string(), proj.flash_baud.to_string(),
                "write_flash".to_string(),
            ];
            if let Some(wf_args) = parsed.get("write_flash_args").and_then(|v| v.as_array()) {
                for x in wf_args { if let Some(s) = x.as_str() { a.push(s.to_string()); } }
            }
            for (addr, file) in &file_args {
                a.push(format!("0x{:x}", addr));
                a.push(file.trim_matches('"').to_string());
            }
            a
        };
        let refs: Vec<&str> = args_unquoted.iter().map(|s| s.as_str()).collect();
        run(&refs, None)?
    };

    if r.code != 0 {
        return Ok(format!("Flash FAILED (esptool):\n{}", summarise_failure(&r.stdout, &r.stderr)));
    }
    Ok(format!("Flash SUCCESS (esptool).\nFlashed {} file(s) to {}.", file_args.len(), proj.flash_port))
}

fn do_flash_idf(proj: &ResolvedProject) -> Result<String> {
    if proj.idf_path.is_empty() {
        return Ok("ERROR: idf_path not configured. Set idf_path in devices.toml.".to_string());
    }
    let build_dir = resolve_build_dir(proj);
    let build_dir_str = build_dir.to_string_lossy().into_owned();

    let port_arg = if !proj.flash_port.is_empty() {
        format!("-p {} ", proj.flash_port)
    } else {
        String::new()
    };
    let baud_arg = if proj.flash_baud > 0 {
        format!("-b {} ", proj.flash_baud)
    } else {
        String::new()
    };
    let cmd = format!(
        "source {}/export.sh 2>/dev/null && idf.py -B {} {}{}flash",
        proj.idf_path, build_dir_str, port_arg, baud_arg
    );
    let r = run(&["bash", "-c", &cmd], Some(Path::new(&proj.project_path)))?;
    if r.code != 0 {
        return Ok(format!("Flash FAILED (idf.py):\n{}", summarise_failure(&r.stdout, &r.stderr)));
    }
    let last: Vec<&str> = r.stdout.lines().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect();
    Ok(format!("Flash SUCCESS (idf.py).\n{}", last.join("\n")))
}

fn do_flash_probe_rs(proj: &ResolvedProject) -> Result<String> {
    let root = Path::new(&proj.project_path);
    let elfs = find_elfs(root);
    if elfs.is_empty() {
        return Ok(format!("No .elf found under {}. Run build_firmware first.", proj.project_path));
    }
    if proj.target.is_empty() {
        return Ok("ERROR: target not configured. Set target in devices.toml for probe-rs flash.".to_string());
    }

    let mut flashed = Vec::new();
    for elf in &elfs {
        let elf_str = elf.to_string_lossy().into_owned();
        let mut args: Vec<&str> = vec!["probe-rs", "download", "--chip", &proj.target];
        if let Some(serial) = &proj.probe_serial { args.extend_from_slice(&["--probe", serial]); }
        args.push(&elf_str);
        let r = run(&args, None)?;
        if r.code != 0 {
            return Ok(format!("Flash FAILED (probe-rs) for {}:\n{}", elf.display(), summarise_failure(&r.stdout, &r.stderr)));
        }
        flashed.push(elf.file_name().unwrap_or_default().to_string_lossy().into_owned());
    }
    Ok(format!("Flash SUCCESS (probe-rs).\nFlashed: {}", flashed.join(", ")))
}

fn do_flash_firmware(proj: &ResolvedProject) -> Result<String> {
    match proj.flash_tool.as_str() {
        "openocd"  => do_flash_openocd(proj),
        "esptool"  => do_flash_esptool(proj),
        "idf"      => do_flash_idf(proj),
        "probe-rs" => do_flash_probe_rs(proj),
        other      => Ok(format!(
            "ERROR: Unknown flash_tool '{}'. Supported: openocd, esptool, idf, probe-rs.", other
        )),
    }
}

// ── MCP server ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct BuildFlashMcp {
    tool_router: ToolRouter<Self>,
    registry: Arc<DeviceRegistry>,
}

impl BuildFlashMcp {
    fn new() -> Self {
        let registry = Arc::new(load_registry());
        Self { tool_router: Self::tool_router(), registry }
    }
}

// ── params structs ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct EmptyParams {}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct BuildParams {
    /// Board alias from devices.toml (e.g. "stm32", "board1"). Fills project_path + preset from registry.
    #[serde(default)]
    board: Option<String>,
    /// Absolute path to CMake project root. Overrides board registry value.
    #[serde(default)]
    project_path: Option<String>,
    /// CMake preset name (e.g. "Debug"). Overrides board registry value.
    #[serde(default)]
    preset: Option<String>,
    /// CMake toolchain file path. Used only when no preset is set.
    #[serde(default)]
    toolchain_file: String,
    /// Build directory relative to project_path when no preset (default: "build").
    #[serde(default = "default_build_dir")]
    build_dir: String,
    /// CMake build type when no preset (default: "Debug").
    #[serde(default = "default_build_type")]
    build_type: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CleanParams {
    /// Board alias from devices.toml.
    #[serde(default)]
    board: Option<String>,
    /// Absolute path to CMake project root. Overrides board registry value.
    #[serde(default)]
    project_path: Option<String>,
    /// CMake preset name — clean build/<preset>. Overrides board registry value.
    #[serde(default)]
    preset: Option<String>,
    /// Build directory to remove when no preset (default: "build").
    #[serde(default = "default_build_dir")]
    build_dir: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SizeParams {
    /// Board alias from devices.toml.
    #[serde(default)]
    board: Option<String>,
    /// Absolute path to CMake project root. Overrides board registry value.
    #[serde(default)]
    project_path: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct FlashParams {
    /// Board alias from devices.toml. Fills project_path + flash config from registry.
    #[serde(default)]
    board: Option<String>,
    /// Absolute path to CMake project root. Overrides board registry value.
    #[serde(default)]
    project_path: Option<String>,
    /// OpenOCD target config (overrides board registry). E.g. "target/stm32wlx.cfg".
    #[serde(default)]
    openocd_config: Option<String>,
}

fn default_build_dir() -> String { "build".to_string() }
fn default_build_type() -> String { "Debug".to_string() }

// ── tools ─────────────────────────────────────────────────────────────────────

#[tool_router]
impl BuildFlashMcp {
    #[tool(description = "List all boards configured in devices.toml with their project paths, presets, and flash tool.")]
    async fn list_projects(&self, _: Parameters<EmptyParams>) -> String {
        if self.registry.board.is_empty() {
            return "No boards configured. Set BUILD_FLASH_MCP_CONFIG to a devices.toml path.".to_string();
        }
        let mut names: Vec<&String> = self.registry.board.keys().collect();
        names.sort();
        let lines: Vec<String> = names.iter().map(|name| {
            let cfg = &self.registry.board[*name];
            let desc = cfg.description.as_deref().unwrap_or("");
            let path = cfg.project_path.as_deref().unwrap_or("(not set)");
            let preset = cfg.preset.as_deref().unwrap_or("Debug");
            let tool = cfg.flash_tool.as_deref().unwrap_or("openocd");
            let target = cfg.target.as_deref().unwrap_or("arm");
            format!("[{}] {}\n  project: {}\n  preset:  {}\n  flash:   {} | target: {}",
                name, desc, path, preset, tool, target)
        }).collect();
        lines.join("\n\n")
    }

    #[tool(description = "Configure (CMake + Ninja) and build firmware. Pass board alias OR project_path + preset. Returns success/failure with key error lines and binary sizes.")]
    async fn build_firmware(&self, Parameters(p): Parameters<BuildParams>) -> String {
        match resolve_project(p.board.as_deref(), p.project_path.as_deref(), p.preset.as_deref(), &self.registry) {
            Ok(proj) => do_build_firmware(&proj, &p.toolchain_file, &p.build_dir, &p.build_type)
                .unwrap_or_else(|e| format!("ERROR: {}", e)),
            Err(e) => e,
        }
    }

    #[tool(description = "Delete the build directory to force a full reconfigure on next build. Pass board alias OR project_path + preset.")]
    async fn clean_build(&self, Parameters(p): Parameters<CleanParams>) -> String {
        match resolve_project(p.board.as_deref(), p.project_path.as_deref(), p.preset.as_deref(), &self.registry) {
            Ok(proj) => do_clean_build(&proj, &p.build_dir),
            Err(e) => e,
        }
    }

    #[tool(description = "Report firmware binary sizes (.text/.data/.bss) from built ELFs. Architecture-aware: uses correct size tool for ARM, Xtensa (ESP32-S3/S2), or RISC-V (ESP32-C3). Pass board alias OR project_path.")]
    async fn get_build_size(&self, Parameters(p): Parameters<SizeParams>) -> String {
        match resolve_project(p.board.as_deref(), p.project_path.as_deref(), None, &self.registry) {
            Ok(proj) => do_get_build_size(&proj),
            Err(e) => e,
        }
    }

    #[tool(description = "Flash all built ELFs to the target. Auto-selects flash tool from board config: openocd (ARM/STM32), esptool (ESP32 via flasher_args.json), idf (ESP-IDF idf.py flash), probe-rs (generic). Pass board alias OR project_path + openocd_config.")]
    async fn flash_firmware(&self, Parameters(p): Parameters<FlashParams>) -> String {
        match resolve_project(p.board.as_deref(), p.project_path.as_deref(), None, &self.registry) {
            Ok(mut proj) => {
                if let Some(cfg) = p.openocd_config { if !cfg.is_empty() { proj.openocd_cfg = cfg; } }
                do_flash_firmware(&proj).unwrap_or_else(|e| format!("ERROR: {}", e))
            }
            Err(e) => e,
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for BuildFlashMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let server = BuildFlashMcp::new();
    let transport = stdio();
    let handle = server.serve(transport).await?;
    handle.waiting().await?;
    Ok(())
}
