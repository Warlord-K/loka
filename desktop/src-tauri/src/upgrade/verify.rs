use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use super::checks::{Launch, ProInfo, ProRelease};

pub(crate) const TOOL_TIMEOUT: Duration = Duration::from_secs(120);
pub(crate) const MAX_LAUNCH_ARGS: usize = 16;
pub(crate) const MAX_LAUNCH_ARG_LEN: usize = 2048;
pub(crate) const APP_NAME: &str = "Loka Pro.app";
const CODESIGN: &str = "/usr/bin/codesign";
const SPCTL: &str = "/usr/sbin/spctl";
const HDIUTIL: &str = "/usr/bin/hdiutil";
const DITTO: &str = "/usr/bin/ditto";
const PLUTIL: &str = "/usr/bin/plutil";
const PGREP: &str = "/usr/bin/pgrep";
const OPEN: &str = "/usr/bin/open";
const LAUNCH_WAIT: Duration = Duration::from_secs(15);
const LAUNCH_POLL: Duration = Duration::from_millis(250);

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    Check(&'static str),
    Running,
    Other,
    Install(String),
}

pub(crate) fn valid_team_id(team: &str) -> bool {
    team.len() == 10 && team.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

pub(crate) fn valid_sha256(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(crate) fn valid_bundle_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 255 && id.split('.').all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
}

pub(crate) fn installable(info: &ProInfo) -> Option<&ProRelease> {
    info.pro.as_ref().filter(|pro| pro.team_id.as_deref().is_some_and(valid_team_id) && valid_sha256(&pro.sha256) && valid_bundle_id(&pro.bundle_id))
}

pub(crate) fn same_origin(url: &str, origin: &str) -> bool {
    match (url::Url::parse(url), url::Url::parse(origin)) {
        (Ok(url), Ok(origin)) => url.username().is_empty() && url.password().is_none() && url.origin() == origin.origin(),
        _ => false,
    }
}

pub(crate) fn launch_args(launch: &Launch) -> Option<Vec<String>> {
    if launch.args.len() > MAX_LAUNCH_ARGS || launch.args.iter().any(|arg| arg.len() > MAX_LAUNCH_ARG_LEN || arg.contains('\0')) {
        return None;
    }
    Some(launch.args.clone())
}

pub(crate) fn open_args(app: &str, launch: &[String]) -> Vec<String> {
    let mut args = vec!["-n".to_string(), "-a".to_string(), app.to_string(), "--args".to_string()];
    args.extend(launch.iter().cloned());
    args
}

pub(crate) fn unix_seconds(text: &str) -> Option<i64> {
    let text = text.trim();
    let (date, rest) = text.split_once(['T', 't', ' '])?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let (clock, offset) = if let Some(clock) = rest.strip_suffix(['Z', 'z']) {
        (clock, 0)
    } else {
        let at = rest.rfind(['+', '-'])?;
        let (clock, zone) = rest.split_at(at);
        let sign = if zone.starts_with('-') { -1 } else { 1 };
        let (hours, minutes) = zone[1..].split_once(':')?;
        (clock, sign * (hours.parse::<i64>().ok()? * 3600 + minutes.parse::<i64>().ok()? * 60))
    };
    let clock = clock.split('.').next()?;
    let mut clock_parts = clock.split(':');
    let hour: i64 = clock_parts.next()?.parse().ok()?;
    let minute: i64 = clock_parts.next()?.parse().ok()?;
    let second: i64 = clock_parts.next()?.parse().ok()?;
    if clock_parts.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + hour * 3600 + minute * 60 + second - offset)
}

pub(crate) fn now_seconds() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

pub(crate) fn still_open(until: Option<&str>, now: i64) -> bool {
    until.and_then(unix_seconds).is_some_and(|until| until > now)
}

pub(crate) fn team_matches(codesign_details: &str, team: &str) -> bool {
    valid_team_id(team) && codesign_details.lines().any(|line| line.trim_end_matches('\r') == format!("TeamIdentifier={team}"))
}

pub(crate) fn notarized(spctl_output: &str) -> bool {
    spctl_output.lines().any(|line| line.trim() == "source=Notarized Developer ID")
}

pub(crate) fn pick_app(found: &[(String, Option<String>)], bundle_id: &str) -> Option<String> {
    let matching: Vec<&String> = found.iter().filter(|(_, id)| id.as_deref() == Some(bundle_id)).map(|(name, _)| name).collect();
    match matching.as_slice() {
        [one] => Some((*one).clone()),
        _ => None,
    }
}

pub(crate) fn plist_value(output: &str) -> Option<String> {
    let value = output.trim();
    (!value.is_empty() && !value.contains('\n')).then(|| value.to_string())
}

pub(crate) fn sized_right(size: u64, expected: Option<u64>) -> bool {
    expected.is_none_or(|expected| expected == size)
}

pub(crate) fn hash_matches(computed: &str, published: &str) -> bool {
    computed.len() == 64 && computed.eq_ignore_ascii_case(published.trim())
}

pub(crate) fn process_pattern(app: &Path) -> String {
    let text = app.join("Contents").join("MacOS").to_string_lossy().into_owned();
    let mut pattern = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        if "\\^$.|?*+()[]{}".contains(c) {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern.push('/');
    pattern
}

pub(crate) fn run_tool(program: &str, args: &[&str]) -> std::io::Result<Output> {
    run_tool_for(program, args, TOOL_TIMEOUT)
}

fn run_tool_for(program: &str, args: &[&str], limit: Duration) -> std::io::Result<Output> {
    let child = Command::new(program)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", std::env::var_os("HOME").unwrap_or_default())
        .env("LANG", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let pid = child.id();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(child.wait_with_output());
    });
    match receiver.recv_timeout(limit) {
        Ok(result) => result,
        Err(_) => {
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGKILL);
            }
            let _ = receiver.recv_timeout(Duration::from_secs(5));
            Err(std::io::Error::new(std::io::ErrorKind::TimedOut, format!("{program} took too long")))
        }
    }
}

fn text(output: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

pub(crate) fn bundle_id(app: &Path) -> Option<String> {
    let plist = app.join("Contents").join("Info.plist");
    if !plist.is_file() {
        return None;
    }
    let plist = plist.to_string_lossy().into_owned();
    let output = run_tool(PLUTIL, &["-extract", "CFBundleIdentifier", "raw", "-o", "-", &plist]).ok()?;
    if !output.status.success() {
        return None;
    }
    plist_value(&String::from_utf8_lossy(&output.stdout))
}

pub(crate) fn check_signature(app: &Path, team: &str) -> Result<(), Refusal> {
    let path = app.to_string_lossy().into_owned();
    let verified = run_tool(CODESIGN, &["--verify", "--deep", "--strict", "--verbose=2", &path]).map_err(|_| Refusal::Check("codesign"))?;
    if !verified.status.success() {
        return Err(Refusal::Check("signature"));
    }
    let details = run_tool(CODESIGN, &["-dv", &path]).map_err(|_| Refusal::Check("codesign"))?;
    if !team_matches(&text(&details), team) {
        return Err(Refusal::Check("team"));
    }
    Ok(())
}

pub(crate) fn check_notarized(app: &Path) -> Result<(), Refusal> {
    let path = app.to_string_lossy().into_owned();
    let assessed = run_tool(SPCTL, &["-a", "-t", "exec", "-vv", &path]).map_err(|_| Refusal::Check("spctl"))?;
    if !assessed.status.success() || !notarized(&text(&assessed)) {
        return Err(Refusal::Check("notarization"));
    }
    Ok(())
}

pub(crate) struct Mounted {
    point: PathBuf,
}

impl Mounted {
    pub(crate) fn attach(dmg: &Path, point: &Path) -> Result<Mounted, Refusal> {
        std::fs::create_dir_all(point).map_err(|_| Refusal::Check("mount"))?;
        let mounted = Mounted { point: point.to_path_buf() };
        let (dmg, at) = (dmg.to_string_lossy().into_owned(), point.to_string_lossy().into_owned());
        let output = run_tool(HDIUTIL, &["attach", "-nobrowse", "-readonly", "-noautoopen", "-mountpoint", &at, &dmg]).map_err(|_| Refusal::Check("mount"))?;
        if !output.status.success() {
            return Err(Refusal::Check("mount"));
        }
        Ok(mounted)
    }

    pub(crate) fn app_for(&self, bundle: &str) -> Result<PathBuf, Refusal> {
        let entries = std::fs::read_dir(&self.point).map_err(|_| Refusal::Check("volume"))?;
        let found: Vec<(String, Option<String>)> = entries
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
            .filter(|name| name.ends_with(".app"))
            .map(|name| {
                let id = bundle_id(&self.point.join(&name));
                (name, id)
            })
            .collect();
        pick_app(&found, bundle).map(|name| self.point.join(name)).ok_or(Refusal::Check("bundle"))
    }
}

impl Drop for Mounted {
    fn drop(&mut self) {
        let at = self.point.to_string_lossy().into_owned();
        let _ = run_tool(HDIUTIL, &["detach", &at, "-force"]);
    }
}

pub(crate) fn writable(dir: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(path) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
        return false;
    };
    dir.is_dir() && unsafe { libc::access(path.as_ptr(), libc::W_OK) == 0 }
}

pub(crate) fn install_target(home: Option<&Path>) -> Option<PathBuf> {
    let system = Path::new("/Applications");
    if writable(system) {
        return Some(system.join(APP_NAME));
    }
    let personal = home?.join("Applications");
    std::fs::create_dir_all(&personal).ok()?;
    writable(&personal).then(|| personal.join(APP_NAME))
}

pub(crate) fn running(app: &Path) -> bool {
    run_tool(PGREP, &["-f", &process_pattern(app)]).is_ok_and(|output| output.status.success())
}

fn to_trash(app: &Path, home: Option<&Path>) -> Result<(), Refusal> {
    let trash = home.map(|home| home.join(".Trash")).ok_or_else(|| Refusal::Install("Loka could not find the Trash.".into()))?;
    std::fs::create_dir_all(&trash).map_err(|_| Refusal::Install("Loka could not use the Trash.".into()))?;
    let stamp = now_seconds();
    let place = trash.join(format!("Loka Pro {stamp}.app"));
    std::fs::rename(app, place).map_err(|_| Refusal::Install("Loka could not move the old Loka Pro to the Trash.".into()))
}

pub(crate) fn make_room(target: &Path, bundle: &str, home: Option<&Path>) -> Result<(), Refusal> {
    if std::fs::symlink_metadata(target).is_err() {
        return Ok(());
    }
    if std::fs::symlink_metadata(target).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(Refusal::Other);
    }
    match bundle_id(target) {
        Some(found) if found == bundle => {
            if running(target) {
                return Err(Refusal::Running);
            }
            to_trash(target, home)
        }
        _ => Err(Refusal::Other),
    }
}

pub(crate) fn partial_path(target: &Path) -> PathBuf {
    let mut name = target.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".partial");
    target.with_file_name(name)
}

pub(crate) fn install(source: &Path, target: &Path, release: &ProRelease, home: Option<&Path>) -> Result<(), Refusal> {
    let team = release.team_id.as_deref().filter(|team| valid_team_id(team)).ok_or(Refusal::Check("team"))?;
    make_room(target, &release.bundle_id, home)?;
    let partial = partial_path(target);
    if std::fs::symlink_metadata(&partial).is_ok() {
        std::fs::remove_dir_all(&partial).map_err(|_| Refusal::Install("Loka could not clear an earlier copy.".into()))?;
    }
    let (from, to) = (source.to_string_lossy().into_owned(), partial.to_string_lossy().into_owned());
    let copied = run_tool(DITTO, &[&from, &to]).map_err(|_| Refusal::Install("Loka could not copy Loka Pro.".into()))?;
    if !copied.status.success() {
        let _ = std::fs::remove_dir_all(&partial);
        return Err(Refusal::Install("Loka could not copy Loka Pro.".into()));
    }
    if let Err(refusal) = check_signature(&partial, team) {
        let _ = std::fs::remove_dir_all(&partial);
        return Err(refusal);
    }
    if bundle_id(&partial).as_deref() != Some(release.bundle_id.as_str()) {
        let _ = std::fs::remove_dir_all(&partial);
        return Err(Refusal::Check("bundle"));
    }
    std::fs::rename(&partial, target).map_err(|_| {
        let _ = std::fs::remove_dir_all(&partial);
        Refusal::Install("Loka could not finish installing Loka Pro.".into())
    })
}

pub(crate) fn launch(app: &Path, args: &[String]) -> bool {
    let path = app.to_string_lossy().into_owned();
    let argv = open_args(&path, args);
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let opened = run_tool_for(OPEN, &argv, Duration::from_secs(30)).is_ok_and(|output| output.status.success());
    if !opened {
        return false;
    }
    let deadline = Instant::now() + LAUNCH_WAIT;
    while Instant::now() < deadline {
        if running(app) {
            return true;
        }
        std::thread::sleep(LAUNCH_POLL);
    }
    false
}

pub(crate) fn reveal(app: &Path) {
    let target = if app.exists() { app.to_path_buf() } else { app.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("/Applications")) };
    let path = target.to_string_lossy().into_owned();
    let _ = run_tool_for(OPEN, &["-R", &path], Duration::from_secs(30));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_args_pass_through_unchanged() {
        let launch = Launch { bundle_id: "dev.ygpt.loka.pro".into(), args: vec!["--loka-handover".into(), "https://loka.ygpt.dev".into(), "--loka-handover-id".into(), "a b;c $(x)".into()] };
        let args = launch_args(&launch).unwrap();
        assert_eq!(args, launch.args);
        assert_eq!(
            open_args("/Applications/Loka Pro.app", &args),
            vec!["-n", "-a", "/Applications/Loka Pro.app", "--args", "--loka-handover", "https://loka.ygpt.dev", "--loka-handover-id", "a b;c $(x)"]
        );
        let too_many = Launch { bundle_id: String::new(), args: vec!["x".into(); MAX_LAUNCH_ARGS + 1] };
        assert!(launch_args(&too_many).is_none());
        let nul = Launch { bundle_id: String::new(), args: vec!["a\0b".into()] };
        assert!(launch_args(&nul).is_none());
    }

    #[test]
    fn download_windows_use_utc_times() {
        assert_eq!(unix_seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(unix_seconds("2026-11-23T08:15:00Z"), Some(1_795_421_700));
        assert_eq!(unix_seconds("2026-11-23T08:15:00.123456Z"), Some(1_795_421_700));
        assert_eq!(unix_seconds("2026-11-23T09:15:00+01:00"), Some(1_795_421_700));
        assert_eq!(unix_seconds("2026-11-23T07:15:00-01:00"), Some(1_795_421_700));
        assert_eq!(unix_seconds("2026-11-23"), None);
        assert_eq!(unix_seconds("2026-13-23T08:15:00Z"), None);
        assert!(still_open(Some("2026-11-23T08:15:00Z"), 1_795_421_699));
        assert!(!still_open(Some("2026-11-23T08:15:00Z"), 1_795_421_700));
        assert!(!still_open(None, 0));
        assert!(now_seconds() > 1_700_000_000);
    }

    #[test]
    fn codesign_details_name_the_team_exactly() {
        let details = "Executable=/Volumes/Loka/Loka.app/Contents/MacOS/Loka\nIdentifier=dev.ygpt.loka.pro\nFormat=app bundle with Mach-O universal\nTeamIdentifier=ABCDE12345\nSealed Resources version=2\n";
        assert!(team_matches(details, "ABCDE12345"));
        assert!(team_matches(&details.replace('\n', "\r\n"), "ABCDE12345"));
        assert!(!team_matches(details, "ABCDE1234"));
        assert!(!team_matches(details, "ZZZZZ99999"));
        assert!(!team_matches("TeamIdentifier=ABCDE12345X\n", "ABCDE12345"));
        assert!(!team_matches("TeamIdentifier=not set\n", "ABCDE12345"));
        assert!(!team_matches(" TeamIdentifier=ABCDE12345\n", "ABCDE12345"));
        assert!(!team_matches("Authority=Developer ID Application: X (ABCDE12345)\n", "ABCDE12345"));
        assert!(!team_matches("TeamIdentifier=\n", ""));
    }

    #[test]
    fn spctl_must_say_notarized_developer_id() {
        assert!(notarized("/Applications/Loka Pro.app: accepted\nsource=Notarized Developer ID\norigin=Developer ID Application: X (ABCDE12345)\n"));
        assert!(!notarized("/Applications/Loka Pro.app: accepted\nsource=Developer ID\n"));
        assert!(!notarized("/Applications/Loka Pro.app: rejected\nsource=Unnotarized Developer ID\n"));
        assert!(!notarized("source=Notarized Developer ID (but not really)\n"));
        assert!(!notarized(""));
    }

    #[test]
    fn the_app_is_found_by_bundle_id_not_name() {
        let found = vec![
            ("Loka.app".to_string(), Some("dev.ygpt.loka.pro".to_string())),
            ("Loka Pro.app".to_string(), Some("dev.ygpt.loka".to_string())),
            ("Other.app".to_string(), None),
        ];
        assert_eq!(pick_app(&found, "dev.ygpt.loka.pro").as_deref(), Some("Loka.app"));
        assert_eq!(pick_app(&found, "dev.ygpt.missing"), None);
        let twice = vec![("A.app".to_string(), Some("dev.ygpt.loka.pro".to_string())), ("B.app".to_string(), Some("dev.ygpt.loka.pro".to_string()))];
        assert_eq!(pick_app(&twice, "dev.ygpt.loka.pro"), None);
        assert_eq!(plist_value("dev.ygpt.loka.pro\n").as_deref(), Some("dev.ygpt.loka.pro"));
        assert_eq!(plist_value(""), None);
        assert_eq!(plist_value("a\nb"), None);
    }

    #[test]
    fn sizes_and_hashes_must_match_what_was_published() {
        assert!(sized_right(10, Some(10)));
        assert!(sized_right(10, None));
        assert!(!sized_right(11, Some(10)));
        let hash = "ab".repeat(32);
        assert!(hash_matches(&hash, &hash));
        assert!(hash_matches(&hash, &format!("{}\n", hash.to_uppercase())));
        assert!(!hash_matches(&hash, &"ac".repeat(32)));
        assert!(!hash_matches("", ""));
    }

    #[test]
    fn downloads_stay_on_the_api_origin() {
        assert!(same_origin("https://loka.ygpt.dev/api/move/pro-dmg?version=0.3.1", "https://loka.ygpt.dev"));
        assert!(!same_origin("https://evil.example/api/move/pro-dmg", "https://loka.ygpt.dev"));
        assert!(!same_origin("http://loka.ygpt.dev/api/move/pro-dmg", "https://loka.ygpt.dev"));
        assert!(!same_origin("https://user@loka.ygpt.dev/x", "https://loka.ygpt.dev"));
    }

    #[test]
    fn the_install_writes_beside_the_target_first() {
        assert_eq!(partial_path(Path::new("/Applications/Loka Pro.app")), PathBuf::from("/Applications/Loka Pro.app.partial"));
        assert_eq!(process_pattern(Path::new("/Applications/Loka Pro.app")), "/Applications/Loka Pro\\.app/Contents/MacOS/");
    }

    #[test]
    fn tools_run_without_a_shell_and_stop_on_time() {
        let output = run_tool("/bin/echo", &["a b", "$(whoami)"]).unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout), "a b $(whoami)\n");
        let started = Instant::now();
        assert!(run_tool_for("/bin/sleep", &["5"], Duration::from_millis(200)).is_err());
        assert!(started.elapsed() < Duration::from_secs(4));
    }

    #[test]
    fn a_missing_or_foreign_app_is_never_replaced() {
        let dir = std::env::temp_dir().join(format!("loka-upgrade-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join(APP_NAME);
        assert_eq!(make_room(&target, "dev.ygpt.loka.pro", None), Ok(()));
        std::fs::create_dir_all(target.join("Contents")).unwrap();
        assert_eq!(make_room(&target, "dev.ygpt.loka.pro", None), Err(Refusal::Other));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn release_ids_have_their_published_shape() {
        assert!(valid_sha256(&"0f".repeat(32)));
        assert!(!valid_sha256(&"0F".repeat(32)));
        assert!(valid_bundle_id("dev.ygpt.loka.pro"));
        assert!(!valid_bundle_id("dev..pro"));
        assert!(!valid_bundle_id("dev.ygpt/../x"));
        assert!(valid_team_id("ABCDE12345"));
        assert!(!valid_team_id("abcde12345"));
        let mut info = ProInfo { pro: Some(ProRelease { version: "0.3.1".into(), sha256: "a".repeat(64), size: None, team_id: Some("ABCDE12345".into()), bundle_id: "dev.ygpt.loka.pro".into(), download_url: String::new(), download_until: None }), ..ProInfo::default() };
        assert!(installable(&info).is_some());
        info.pro.as_mut().unwrap().team_id = None;
        assert!(installable(&info).is_none());
        info.pro = None;
        assert!(installable(&info).is_none());
    }
}
