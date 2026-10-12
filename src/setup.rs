//! Files patrak installs outside its own data directory.

use crate::config::{self, Config, Paths};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const APP_ID: &str = "dev.patrak.Patrak";
const MPV_SCRIPT: &str = include_str!("../assets/patrak.lua");
const DESKTOP_ENTRY: &str = include_str!("../assets/dev.patrak.Patrak.desktop");
const TIMER_UNIT: &str = "patrak-remind.timer";

#[derive(Debug)]
pub enum Outcome {
    Written(PathBuf),
    Kept(PathBuf),
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Outcome::Written(p) => write!(f, "wrote   {}", p.display()),
            Outcome::Kept(p) => write!(f, "kept    {} (exists; --force to overwrite)", p.display()),
        }
    }
}

fn write(path: PathBuf, content: &str, force: bool) -> Result<Outcome> {
    if path.exists() && !force {
        return Ok(Outcome::Kept(path));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, content).with_context(|| format!("writing {}", path.display()))?;
    Ok(Outcome::Written(path))
}

pub fn write_config(paths: &Paths, cfg: &Config, force: bool) -> Result<Outcome> {
    write(paths.config.clone(), &toml::to_string_pretty(cfg)?, force)
}

pub fn create_library_dirs(cfg: &Config) -> Result<Vec<PathBuf>> {
    let dirs: Vec<PathBuf> = cfg
        .video_roots()
        .into_iter()
        .chain(cfg.book_roots())
        .chain(cfg.project_roots())
        .collect();
    for d in &dirs {
        std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
    }
    Ok(dirs)
}

pub fn install_mpv_script(paths: &Paths, force: bool) -> Result<Outcome> {
    // The script appends to this log but can't create its directory.
    if let Some(dir) = paths.events_log.parent() {
        std::fs::create_dir_all(dir)?;
    }
    write(
        config::config_home().join("mpv/scripts/patrak.lua"),
        MPV_SCRIPT,
        force,
    )
}

pub fn install_desktop_entry(force: bool) -> Result<Outcome> {
    let exe = exe()?;
    let dir = dirs::data_dir()
        .context("no data directory")?
        .join("applications");
    write(
        dir.join(format!("{APP_ID}.desktop")),
        &DESKTOP_ENTRY.replace("{exe}", &quote(&exe)),
        force,
    )
}

/// Writes the reminder service and timer and (re)starts the timer. Always overwrites, so
/// changing `reminder_time` takes effect.
pub fn install_reminder(cfg: &Config) -> Result<Vec<Outcome>> {
    let time = config::parse_hhmm(&cfg.reminder_time)
        .with_context(|| format!("reminder_time {:?} is not HH:MM", cfg.reminder_time))?;
    let dir = config::config_home().join("systemd/user");
    let service = format!(
        "[Unit]\nDescription=patrak daily learning reminder\n\n[Service]\nType=oneshot\nExecStart={} remind\n",
        quote(&exe()?)
    );
    let timer = format!(
        "[Unit]\nDescription=patrak daily learning reminder\n\n[Timer]\nOnCalendar=*-*-* {}:00\nPersistent=true\n\n[Install]\nWantedBy=timers.target\n",
        time.format("%H:%M")
    );
    let written = vec![
        write(dir.join("patrak-remind.service"), &service, true)?,
        write(dir.join(TIMER_UNIT), &timer, true)?,
    ];
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", TIMER_UNIT])?;
    systemctl(&["restart", TIMER_UNIT])?;
    Ok(written)
}

pub fn reminder_installed() -> bool {
    config::config_home()
        .join("systemd/user")
        .join(TIMER_UNIT)
        .exists()
}

fn systemctl(args: &[&str]) -> Result<()> {
    let status = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()
        .context("running systemctl")?;
    if !status.success() {
        bail!("systemctl --user {} failed", args.join(" "));
    }
    Ok(())
}

fn exe() -> Result<PathBuf> {
    std::env::current_exe().context("locating the patrak binary")
}

/// Quotes a path for a desktop entry or systemd unit if it contains spaces.
fn quote(p: &Path) -> String {
    let s = p.to_string_lossy();
    if s.contains(' ') {
        format!("\"{s}\"")
    } else {
        s.into_owned()
    }
}
