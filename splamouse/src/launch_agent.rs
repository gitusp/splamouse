//! ログイン時に splamouse をバックグラウンドで起動する LaunchAgent の登録と解除。

use crate::config;
use anyhow::{Context, Result, ensure};
use std::{
    env,
    ffi::OsStr,
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const LABEL: &str = "com.github.gitusp.splamouse";

pub fn install() -> Result<()> {
    // 誤った設定のまま常駐させないよう、先に読めるか確かめる。
    config::load()?;
    // launchd から起動するとシェルの環境変数は引き継がれないので、同じ設定ファイルを読むよう渡す。
    let config_home = config::xdg_config_home();
    let config_home = config_home
        .as_deref()
        .map(|dir| dir.to_str().context("XDG_CONFIG_HOME isn't valid UTF-8"))
        .transpose()?;

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
    fs::write(&plist, render_plist(exe, config_home, log))
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
    if let Some(path) = config::path() {
        println!("- Config: {}", path.display());
        println!(
            "  Restart splamouse after editing it: launchctl kickstart -k {}",
            service_target()
        );
    }
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

fn render_plist(exe: &str, config_home: Option<&str>, log: &str) -> String {
    let exe = xml_escape(exe);
    let log = xml_escape(log);
    let environment = config_home
        .map(|dir| {
            format!(
                "\t<key>EnvironmentVariables</key>\n\t<dict>\n\t\t<key>XDG_CONFIG_HOME</key>\n\t\t<string>{}</string>\n\t</dict>\n",
                xml_escape(dir)
            )
        })
        .unwrap_or_default();
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
	</array>
{environment}	<key>RunAtLoad</key>
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
    fn plist_escapes_paths() {
        let plist = render_plist("/Users/a&b/<splamouse>", None, "/tmp/log");
        assert!(plist.contains("<string>/Users/a&amp;b/&lt;splamouse&gt;</string>"));
        assert!(!plist.contains("EnvironmentVariables"));
    }

    #[test]
    fn plist_passes_xdg_config_home() {
        let plist = render_plist("/bin/splamouse", Some("/Users/a&b/.conf"), "/tmp/log");
        assert!(plist.contains(
            "\t<key>EnvironmentVariables</key>\n\t<dict>\n\t\t<key>XDG_CONFIG_HOME</key>\n\t\t<string>/Users/a&amp;b/.conf</string>\n\t</dict>\n\t<key>RunAtLoad</key>"
        ));
    }
}
