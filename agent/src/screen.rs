use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use uuid::Uuid;

pub const LAUNCHER_ENV: &str = "WORLD_SCREEN_LAUNCHER";
const SCREENCAPTURE: &str = "/usr/sbin/screencapture";
const SWITCH_FILE: &str = "screen.on";
const MAX_RECORD_SECS: u32 = 60;

static ALLOWED: AtomicBool = AtomicBool::new(false);

pub fn installed() -> bool {
    cfg!(target_os = "macos")
        && Path::new(SCREENCAPTURE).is_file()
        && std::env::var("WORLD_SCREEN").map_or(true, |v| !matches!(v.trim(), "off" | "0" | "false"))
}

pub fn allowed() -> bool {
    installed() && ALLOWED.load(Ordering::Relaxed)
}

pub fn set_allowed(root: &Path, on: bool) {
    let on = on && installed();
    if ALLOWED.swap(on, Ordering::Relaxed) == on && switch_file(root).exists() == on {
        return;
    }
    let file = switch_file(root);
    if on {
        let _ = std::fs::create_dir_all(root.join(crate::browser::MEDIA_DIR));
        let _ = std::fs::write(&file, b"on");
    } else {
        let _ = std::fs::remove_file(&file);
    }
}

fn switch_file(root: &Path) -> PathBuf {
    root.join(crate::browser::MEDIA_DIR).join(SWITCH_FILE)
}

pub fn wanted(kind: &str) -> bool {
    matches!(kind, "task" | "followup")
}

fn launcher_exe() -> Option<String> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let exe = exe.to_str()?.to_string();
    exe.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+".contains(c)).then_some(exe)
}

pub fn launcher(agent_id: Uuid) -> Option<String> {
    Some(format!("{} screen {agent_id}", launcher_exe()?))
}

pub fn excluded_commands(launcher: &str) -> Vec<String> {
    vec![launcher.to_string(), format!("{launcher}:*"), format!("{launcher} *")]
}

pub fn skill(launcher: &str) -> String {
    format!(
        "---
name: screen
description: Take a screenshot or a short screen recording of your owner's Mac to show them something outside the browser.
---
# Screen

Your owner allowed you to capture this Mac's screen. Run it with this exact prefix, written out in full every time, as a command on its own (no pipe, `&&`, `;` or `cd`, or the sandbox keeps it from working):

`{launcher}`

- `{launcher} shot` takes a screenshot of the main display.
- `{launcher} region <x>,<y>,<width>,<height>` captures one area, in points from the top left.
- `{launcher} record <seconds>` records the main display for up to {MAX_RECORD_SECS} seconds, then saves it.

Each prints where it saved the file. Pass that path to the `show_media` tool to show it: collapsed for in-between
steps, open for the result.

Rules:
- Capture only to show your owner what you did or found: an app you changed, a run you watched, a bug you saw.
- The screen can hold private things (messages, mail, other people's names). Prefer a region over the whole screen,
  and never send a capture anywhere except `show_media`.
- If it fails with a permission error, say in your report that your owner needs to allow Loka under
  System Settings > Privacy & Security > Screen Recording, then restart the Loka app.
"
    )
}

#[derive(Debug, PartialEq, Eq)]
pub enum Capture {
    Shot,
    Region(u32, u32, u32, u32),
    Record(u32),
}

pub fn parse(args: &[String]) -> Result<Capture, String> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["shot"] => Ok(Capture::Shot),
        ["region", spec] => {
            let parts: Vec<u32> = spec.split(',').map(|p| p.trim().parse::<u32>()).collect::<Result<_, _>>().map_err(|_| "region is x,y,width,height in whole numbers".to_string())?;
            match parts.as_slice() {
                [x, y, w, h] if *w > 0 && *h > 0 && *w <= 20000 && *h <= 20000 => Ok(Capture::Region(*x, *y, *w, *h)),
                _ => Err("region is x,y,width,height with a width and height above zero".into()),
            }
        }
        ["record", secs] => match secs.parse::<u32>() {
            Ok(n) if (1..=MAX_RECORD_SECS).contains(&n) => Ok(Capture::Record(n)),
            _ => Err(format!("record takes 1 to {MAX_RECORD_SECS} seconds")),
        },
        _ => Err("use shot, region <x>,<y>,<width>,<height> or record <seconds>".into()),
    }
}

fn capture_args(capture: &Capture, file: &Path) -> Vec<String> {
    let mut out = vec!["-x".to_string()];
    match capture {
        Capture::Shot => out.extend(["-t".into(), "png".into()]),
        Capture::Region(x, y, w, h) => out.extend(["-t".into(), "png".into(), format!("-R{x},{y},{w},{h}")]),
        Capture::Record(secs) => out.extend(["-v".into(), format!("-V{secs}")]),
    }
    out.push(file.display().to_string());
    out
}

pub fn gate(args: &[String]) -> i32 {
    match run(args) {
        Ok(path) => {
            println!("Saved to {}", path.display());
            0
        }
        Err(message) => {
            eprintln!("screen: {message}");
            2
        }
    }
}

fn run(args: &[String]) -> Result<PathBuf, String> {
    let [agent, rest @ ..] = args else {
        return Err("missing agent".into());
    };
    let agent_id: Uuid = agent.parse().map_err(|_| "bad agent id".to_string())?;
    if !installed() {
        return Err("screen capture only works on a Mac".into());
    }
    let root = crate::browser::runner_root().ok_or("screen capture only runs inside a Loka job")?;
    if !switch_file(&root).is_file() {
        return Err("your owner has not allowed screen capture on this Mac (Devices > Let agents see this screen)".into());
    }
    let capture = parse(rest)?;
    let dir = crate::browser::media_dir(&root, agent_id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not prepare the media folder: {e}"))?;
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let ext = if matches!(capture, Capture::Record(_)) { "mov" } else { "png" };
    let kind = if matches!(capture, Capture::Record(_)) { "recording" } else { "shot" };
    let file = dir.join(format!("screen-{kind}-{stamp}-{}.{ext}", &Uuid::new_v4().simple().to_string()[..6]));
    let output = Command::new(SCREENCAPTURE)
        .args(capture_args(&capture, &file))
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not start screencapture: {e}"))?;
    let empty = std::fs::metadata(&file).map(|m| m.len() == 0).unwrap_or(true);
    if !output.status.success() || empty {
        let _ = std::fs::remove_file(&file);
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let sandbox = if crate::browser::inside_sandbox() { format!(" {}", crate::browser::RUN_ALONE) } else { String::new() };
        return Err(format!(
            "the capture failed{}. Loka may need Screen Recording permission: System Settings > Privacy & Security > Screen Recording, then restart Loka.{sandbox}",
            if detail.is_empty() { String::new() } else { format!(" ({detail})") }
        ));
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn parses_only_known_captures() {
        assert_eq!(parse(&words("shot")), Ok(Capture::Shot));
        assert_eq!(parse(&words("region 10,20,300,200")), Ok(Capture::Region(10, 20, 300, 200)));
        assert_eq!(parse(&words("record 15")), Ok(Capture::Record(15)));
        for bad in ["", "shot /tmp/x.png", "record 0", "record 600", "region 1,2,3", "region 0,0,0,10", "window Safari", "shot; rm -rf ~"] {
            assert!(parse(&words(bad)).is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn captures_stay_silent_and_go_to_the_given_file() {
        let file = Path::new("/r/media/a/x.png");
        assert_eq!(capture_args(&Capture::Shot, file), words("-x -t png /r/media/a/x.png"));
        assert_eq!(capture_args(&Capture::Region(1, 2, 3, 4), file), words("-x -t png -R1,2,3,4 /r/media/a/x.png"));
        assert_eq!(capture_args(&Capture::Record(5), file), words("-x -v -V5 /r/media/a/x.png"));
    }

    #[test]
    fn needs_the_switch_file() {
        assert!(run(&[Uuid::new_v4().to_string(), "shot".into()]).is_err());
    }
}
