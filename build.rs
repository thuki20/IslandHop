//! 把岛仔图标作为 RT_GROUP_ICON #1 编进 exe，供 LoadImageW(hInst, 1, ...) 取用。
//! 不引入额外依赖：直接调用 Windows SDK 的 rc.exe，把 .res 交给链接器。

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn find_rc() -> Option<PathBuf> {
    let roots = [
        r"C:\Program Files (x86)\Windows Kits\10\bin",
        r"C:\Program Files\Windows Kits\10\bin",
    ];
    let mut found: Vec<PathBuf> = Vec::new();
    for root in roots {
        let Ok(entries) = fs::read_dir(root) else { continue };
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let rc = dir.join("x64").join("rc.exe");
            if rc.exists() {
                found.push(rc);
            }
        }
    }
    found.sort();
    found.pop()
}

/// 无 Windows SDK 时的兜底：GNU 工具链自带的 windres 也能编 RC。
fn find_windres() -> Option<PathBuf> {
    let names = ["windres.exe", "x86_64-w64-mingw32-windres.exe"];
    if let Some(path) = env::var_os("PATH") {
        for dir in env::split_paths(&path) {
            for name in names {
                let exe = dir.join(name);
                if exe.exists() {
                    return Some(exe);
                }
            }
        }
    }
    None
}

fn main() {
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=packaging/assets/AppIcon.ico");
    println!("cargo:rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("daozai.res");
    let Some(rc) = find_rc().or_else(find_windres) else {
        println!("cargo:warning=rc.exe / windres 都未找到，跳过图标资源");
        return;
    };
    // 版本资源按 Cargo.toml 的版本号现生成：app.rc 里的版本是占位符，
    // 以前写死成 0.6.64，每次发版 Windows 包都会因为「Executable version
    // mismatch」打不出来（scripts/package.ps1 会校验 FileVersion）。
    let version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    // FILEVERSION 要四位：0.6.75 → 0,6,75,0
    let mut parts: Vec<String> = version.split('.').map(str::to_owned).collect();
    while parts.len() < 4 {
        parts.push("0".into());
    }
    let comma = parts.join(",");
    let template = fs::read_to_string("app.rc").unwrap_or_default();
    let generated = template
        .replace("@VERSION_COMMA@", &comma)
        .replace("@VERSION_DOT@", &version);
    let rc_source = PathBuf::from(env::var("OUT_DIR").unwrap()).join("app.versioned.rc");
    if fs::write(&rc_source, generated).is_err() {
        println!("cargo:warning=写入版本资源失败");
        return;
    }
    let is_windres = rc
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.contains("windres"));
    let status = if is_windres {
        // windres：输入 RC，输出 COFF 目标文件（GNU ld 直接链接）。
        Command::new(&rc)
            .arg("-O")
            .arg("coff")
            .arg(&rc_source)
            .arg(&out)
            .status()
    } else {
        Command::new(&rc)
            .arg("/nologo")
            .arg("/fo")
            .arg(&out)
            .arg(&rc_source)
            .status()
    };
    if !status.map(|s| s.success()).unwrap_or(false) {
        println!("cargo:warning=资源编译器（{}）编译 app.rc 失败", rc.display());
        return;
    }
    println!("cargo:rustc-link-arg={}", out.display());
}
