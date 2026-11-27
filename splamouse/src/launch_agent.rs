//! ログイン時に splamouse をバックグラウンドで起動する LaunchAgent の登録と解除。

use anyhow::{Context, Result, ensure};
use std::{
    env,
    ffi::OsStr,
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const LABEL: &str = "com.github.gitusp.splamouse";

pub fn install(gyro: f64, stick: f64) -> Result<()> {
    let exe = env::current_exe()
        .and_then(|path| path.canonicalize())
        .context("error locating the splamouse binary")?;
    let exe = exe
        .to_str()
        .context("the path to splamouse isn't valid UTF-8")?;
    let home = home_dir()?;
    let plist = plist_path(&home);
    let log = home.join("Library/Logs/splamouse.log");
    let log = log
        .to_str()
        .context("the path to the log file isn't valid UTF-8")?;

    fs::create_dir_all(plist.parent().unwrap()).context("error creating ~/Library/LaunchAgents")?;
    fs::write(&plist, render_plist(exe, gyro, stick, log))
        .with_context(|| format!("error writing {}", plist.display()))?;

    // 登録済みなら、新しい設定で起動し直すために一度外す。
    bootout_if_loaded()?;
    launchctl([
        OsStr::new("bootstrap"),
        OsStr::new(&domain_target()),
        plist.as_os_str(),
    ])?;

    println!("splamouse now runs in the background, and will start at every login.");
    println!();
    println!(
        "- If asked, allow splamouse in System Settings > Privacy & Security > Accessibility."
    );
    println!("- Keep the binary where it is: {exe}");
    println!("  Run `splamouse install` again after moving or updating it.");
    println!("- Logs: {log}");
    println!("- To stop it: splamouse uninstall");
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let plist = plist_path(&home_dir()?);

    bootout_if_loaded()?;
    match fs::remove_file(&plist) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("error removing {}", plist.display())),
    }

    println!("splamouse no longer runs in the background.");
    Ok(())
}

fn home_dir() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME isn't set")
}

fn plist_path(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

/// ログイン中のユーザーの GUI セッション。マウスやキーボードの操作はここからでないと送れない。
fn domain_target() -> String {
    // SAFETY: getuid は常に成功し、副作用も無い。
    format!("gui/{}", unsafe { libc::getuid() })
}

fn service_target() -> String {
    format!("{}/{LABEL}", domain_target())
}

/// 登録済みなら外す。動いている splamouse も止まる。
fn bootout_if_loaded() -> Result<()> {
    let loaded = Command::new("launchctl")
        .arg("print")
        .arg(service_target())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if loaded {
        launchctl(["bootout", &service_target()])?;
    }
    Ok(())
}

fn launchctl<S: AsRef<OsStr>>(args: impl IntoIterator<Item = S>) -> Result<()> {
    let mut command = Command::new("launchctl");
    command.args(args);
    let status = command
        .status()
        .with_context(|| format!("error running {command:?}"))?;
    ensure!(status.success(), "{command:?} failed ({status})");
    Ok(())
}

fn render_plist(exe: &str, gyro: f64, stick: f64, log: &str) -> String {
    let exe = xml_escape(exe);
    let log = xml_escape(log);
    // RunAtLoad + KeepAlive: ログイン時に起動し、落ちても起動し直す。
    // ProcessType Interactive: バックグラウンド扱いで間引かれて、マウス操作が遅れないようにする。
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{exe}</string>
		<string>--gyro={gyro}</string>
		<string>--stick={stick}</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<true/>
	<key>ProcessType</key>
	<string>Interactive</string>
	<key>StandardOutPath</key>
	<string>{log}</string>
	<key>StandardErrorPath</key>
	<string>{log}</string>
</dict>
</plist>
"#
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_escapes_paths_and_keeps_sensitivity() {
        let plist = render_plist("/Users/a&b/<splamouse>", -3.0, 4.5, "/tmp/log");
        assert!(plist.contains("<string>/Users/a&amp;b/&lt;splamouse&gt;</string>"));
        assert!(plist.contains("<string>--gyro=-3</string>"));
        assert!(plist.contains("<string>--stick=4.5</string>"));
    }
}
