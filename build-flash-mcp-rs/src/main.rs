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
    path::{Path, PathBuf},
    process::{Command, Output},
};

const OPENOCD_DEFAULT_CFG: &str =
    "/opt/homebrew/Cellar/open-ocd/0.12.0_1/share/openocd/scripts/target/stm32wlx.cfg";

// ── helpers ──────────────────────────────────────────────────────────────────

struct RunResult {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run(args: &[&str], cwd: Option<&Path>) -> Result<RunResult> {
    let mut cmd = Command::new(args[0]);
    cmd.args(&args[1..]);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
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
        .into_iter()
        .flatten()
        .flatten()
        .collect();
    elfs.sort();
    elfs
}

fn summarise_failure(out: &str, err: &str) -> String {
    let errors = extract_errors(err)
        .into_iter()
        .chain(extract_errors(out))
        .collect::<Vec<_>>();
    if !errors.is_empty() {
        errors[..errors.len().min(15)].join("\n")
    } else {
        err.lines()
            .rev()
            .take(10)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n")
    }
}

// ── tool implementations ─────────────────────────────────────────────────────

fn do_build_firmware(
    project_path: &str,
    preset: &str,
    toolchain_file: &str,
    build_dir: &str,
    build_type: &str,
) -> Result<String> {
    let proj = Path::new(project_path);

    if !preset.is_empty() {
        let presets_file = proj.join("CMakePresets.json");
        if !presets_file.exists() {
            return Ok(format!("ERROR: CMakePresets.json not found in {}", project_path));
        }

        let build_path = proj.join("build").join(preset);
        if !build_path.join("CMakeCache.txt").exists() {
            let r = run(&["cmake", "--preset", preset], Some(proj))?;
            if r.code != 0 {
                return Ok(format!("Configure FAILED. Key errors:\n{}", summarise_failure(&r.stdout, &r.stderr)));
            }
        }

        let r = run(&["cmake", "--build", "--preset", preset], Some(proj))?;
        if r.code != 0 {
            return Ok(format!("Build FAILED. Key errors:\n{}", summarise_failure(&r.stdout, &r.stderr)));
        }
    } else {
        if toolchain_file.is_empty() {
            return Ok("ERROR: Provide either a preset name or a toolchain_file path.".to_string());
        }
        let build_path = proj.join(build_dir);
        let build_path_str = build_path.to_string_lossy();

        if !build_path.join("CMakeCache.txt").exists() {
            let r = run(&[
                "cmake",
                "-B", &build_path_str,
                "-G", "Ninja",
                &format!("-DCMAKE_TOOLCHAIN_FILE={}", toolchain_file),
                &format!("-DCMAKE_BUILD_TYPE={}", build_type),
                project_path,
            ], None)?;
            if r.code != 0 {
                return Ok(format!("Configure FAILED. Key errors:\n{}", summarise_failure(&r.stdout, &r.stderr)));
            }
        }

        let r = run(&["cmake", "--build", &build_path_str, "--", "-j4"], None)?;
        if r.code != 0 {
            return Ok(format!("Build FAILED. Key errors:\n{}", summarise_failure(&r.stdout, &r.stderr)));
        }
    }

    let elfs = find_elfs(proj);
    let mut size_info = String::new();
    if !elfs.is_empty() {
        let mut lines = Vec::new();
        for elf in &elfs {
            let r = run(&["arm-none-eabi-size", &elf.to_string_lossy()], None)?;
            if r.code == 0 {
                let name = elf.file_name().unwrap_or_default().to_string_lossy();
                lines.push(format!("{}:\n{}", name, r.stdout.trim()));
            }
        }
        if !lines.is_empty() {
            size_info = format!("\nBinary sizes:\n{}", lines.join("\n\n"));
        }
    }

    Ok(format!("Build SUCCESS.{}", size_info))
}

fn do_clean_build(project_path: &str, build_dir: &str) -> String {
    let build_path = Path::new(project_path).join(build_dir);
    if !build_path.exists() {
        return format!("Nothing to clean — {} does not exist.", build_path.display());
    }
    match std::fs::remove_dir_all(&build_path) {
        Ok(_) => format!("Cleaned {}. Next build will reconfigure.", build_path.display()),
        Err(e) => format!("ERROR: Could not remove {}: {}", build_path.display(), e),
    }
}

fn do_get_build_size(project_path: &str) -> Result<String> {
    let proj = Path::new(project_path);
    let elfs = find_elfs(proj);
    if elfs.is_empty() {
        return Ok(format!("No .elf file found under {}. Run build_firmware first.", project_path));
    }
    let mut lines = Vec::new();
    for elf in &elfs {
        let r = run(&["arm-none-eabi-size", &elf.to_string_lossy()], None)?;
        if r.code == 0 {
            let name = elf.file_name().unwrap_or_default().to_string_lossy();
            lines.push(format!("{}:\n{}", name, r.stdout.trim()));
        }
    }
    if lines.is_empty() {
        Ok("arm-none-eabi-size failed on all ELFs.".to_string())
    } else {
        Ok(lines.join("\n\n"))
    }
}

fn do_flash_firmware(project_path: &str, openocd_config: &str) -> Result<String> {
    let proj = Path::new(project_path);
    let elfs = find_elfs(proj);
    if elfs.is_empty() {
        return Ok(format!("No .elf file found under {}. Run build_firmware first.", project_path));
    }

    let program_cmds: String = elfs
        .iter()
        .map(|e| format!("program {{{}}} verify;", e.display()))
        .collect::<Vec<_>>()
        .join(" ");
    let openocd_cmd = format!("{} reset halt; resume; exit", program_cmds);

    let r = run(&[
        "openocd",
        "-f", "interface/stlink.cfg",
        "-f", openocd_config,
        "-c", &openocd_cmd,
    ], None)?;

    let combined = format!("{}{}", r.stdout, r.stderr).trim().to_string();

    if r.code != 0 {
        let errors = extract_errors(&combined);
        let msg = if !errors.is_empty() {
            errors[..errors.len().min(10)].join("\n")
        } else {
            combined.lines().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")
        };
        return Ok(format!("Flash FAILED:\n{}", msg));
    }

    let flashed: Vec<String> = elfs
        .iter()
        .map(|e| e.file_name().unwrap_or_default().to_string_lossy().into_owned())
        .collect();

    if combined.to_lowercase().contains("verified") {
        Ok(format!("Flash SUCCESS. Verified and reset.\nFlashed: {}", flashed.join(", ")))
    } else {
        Ok(format!("Flash completed (verify status unclear).\nOutput:\n{}", &combined[combined.len().saturating_sub(300)..]))
    }
}

// ── MCP server ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct BuildFlashMcp {
    tool_router: ToolRouter<Self>,
}

impl BuildFlashMcp {
    fn new() -> Self {
        Self { tool_router: Self::tool_router() }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct BuildParams {
    /// Absolute path to the CMake project root (where CMakeLists.txt lives)
    project_path: String,
    /// CMake preset name from CMakePresets.json (e.g. "Debug"). If set, other fields are ignored.
    #[serde(default)]
    preset: String,
    /// Absolute path to the CMake toolchain file. Used only when no preset.
    #[serde(default)]
    toolchain_file: String,
    /// Build output directory relative to project_path (default: "build")
    #[serde(default = "default_build_dir")]
    build_dir: String,
    /// CMake build type (default: "Debug")
    #[serde(default = "default_build_type")]
    build_type: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CleanParams {
    /// Absolute path to the CMake project root
    project_path: String,
    /// Build directory name to remove (default: "build")
    #[serde(default = "default_build_dir")]
    build_dir: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SizeParams {
    /// Absolute path to the CMake project root
    project_path: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct FlashParams {
    /// Absolute path to the CMake project root
    project_path: String,
    /// Path to the OpenOCD target config file (default: stm32wlx.cfg)
    #[serde(default = "default_openocd_cfg")]
    openocd_config: String,
}

fn default_build_dir() -> String { "build".to_string() }
fn default_build_type() -> String { "Debug".to_string() }
fn default_openocd_cfg() -> String { OPENOCD_DEFAULT_CFG.to_string() }

#[tool_router]
impl BuildFlashMcp {
    #[tool(description = "Configure (CMake + Ninja) and build a firmware project. Supports CMakePresets.json (preferred) and manual toolchain file. Returns semantic summary of success or failure with key error lines.")]
    async fn build_firmware(&self, Parameters(p): Parameters<BuildParams>) -> String {
        do_build_firmware(&p.project_path, &p.preset, &p.toolchain_file, &p.build_dir, &p.build_type)
            .unwrap_or_else(|e| format!("ERROR: {}", e))
    }

    #[tool(description = "Delete the build directory to force a full reconfigure on next build.")]
    async fn clean_build(&self, Parameters(p): Parameters<CleanParams>) -> String {
        do_clean_build(&p.project_path, &p.build_dir)
    }

    #[tool(description = "Report firmware binary size (.text / .data / .bss) from the built ELF.")]
    async fn get_build_size(&self, Parameters(p): Parameters<SizeParams>) -> String {
        do_get_build_size(&p.project_path)
            .unwrap_or_else(|e| format!("ERROR: {}", e))
    }

    #[tool(description = "Flash all built ELFs to the target via OpenOCD and ST-Link V3. Supports dual-core projects — programs all ELFs in a single OpenOCD session.")]
    async fn flash_firmware(&self, Parameters(p): Parameters<FlashParams>) -> String {
        do_flash_firmware(&p.project_path, &p.openocd_config)
            .unwrap_or_else(|e| format!("ERROR: {}", e))
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
