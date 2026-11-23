use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::sync::OnceLock;

pub const QUEST_RUN_PREFIXES: &[&str] = &["CLAUDE_CODE_", "GIT_", "CARGO_PROFILE_"];

pub const QUEST_RUN_NAMES: &[&str] = &["CARGO_TARGET_DIR", "CARGO_INCREMENTAL", "CLAUDE_CODE_TMPDIR", "CLAUDE_CODE_SHELL"];

pub const REAL_SHELL: &str = "LOKA_REAL_SHELL";

pub const PRIVATE_TEMP_ROOT: &str = "PRIVATE_TEMP_ROOT";

pub const QUEST_TEMP: &str = "CLAUDE_CODE_TMPDIR";

pub const QUEST_TEMP_PREFIXES: &[&str] = &["loka-q-", "loka-s-"];

pub fn from_quest_run(name: &str) -> bool {
    QUEST_RUN_NAMES.contains(&name)
        || loka_shared::loka_env::RESERVED_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
        || (QUEST_RUN_PREFIXES.iter().any(|prefix| name.starts_with(prefix)) && !crate::passed_on_to_clis(name))
}

fn quest_temp_dir(value: &OsStr) -> Option<OsString> {
    Path::new(value)
        .ancestors()
        .find(|dir| dir.file_name().and_then(OsStr::to_str).is_some_and(|name| QUEST_TEMP_PREFIXES.iter().any(|prefix| name.starts_with(prefix))))
        .map(|dir| dir.as_os_str().to_owned())
}

pub fn changes(vars: impl IntoIterator<Item = (OsString, OsString)>) -> Vec<(OsString, Option<OsString>)> {
    let mut changes = Vec::new();
    let mut restored = Vec::new();
    for (name, value) in vars {
        if name.to_str().is_some_and(from_quest_run) {
            if name == REAL_SHELL {
                restored.push(("SHELL".into(), Some(value.clone())));
            }
            if name == QUEST_TEMP {
                restored.extend(quest_temp_dir(&value).map(|dir| ("TMPDIR".into(), Some(dir))));
            }
            changes.push((name, None));
        }
    }
    changes.extend(restored);
    changes
}

static HOST: OnceLock<Vec<(OsString, OsString)>> = OnceLock::new();

static STARTED_WITH: OnceLock<Vec<OsString>> = OnceLock::new();

#[allow(dead_code)]
pub fn host_var(name: &str) -> Option<String> {
    HOST.get()?.iter().find(|(key, _)| key == OsStr::new(name)).and_then(|(_, value)| value.to_str()).map(String::from)
}

extern "C" fn start_clean() {
    let host: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    for (name, value) in changes(host.iter().cloned()) {
        match value {
            Some(value) => std::env::set_var(name, value),
            None => std::env::remove_var(name),
        }
    }
    let _ = HOST.set(host);
    let _ = STARTED_WITH.set(std::env::vars_os().map(|(name, _)| name).collect());
    if let Some(temp) = std::env::var_os("TMPDIR").filter(|temp| !temp.is_empty()) {
        std::env::set_var(format!("LOKA_{PRIVATE_TEMP_ROOT}"), temp);
    }
}

#[used]
#[cfg_attr(target_vendor = "apple", link_section = "__DATA,__mod_init_func")]
#[cfg_attr(not(target_vendor = "apple"), link_section = ".init_array")]
static START_CLEAN: extern "C" fn() = start_clean;

#[cfg(test)]
mod tests {
    use super::*;

    fn quest_like() -> Vec<(OsString, OsString)> {
        [
            ("PATH", "/usr/bin:/bin"),
            ("HOME", "/Users/someone"),
            ("TMPDIR", "/tmp/claude-501"),
            ("SHELL", "/var/folders/x/T/loka-cmd-hub-1/bash"),
            ("CLAUDE_CODE_TMPDIR", "/var/folders/x/T/loka-q-0123456789ab/claude-501"),
            ("CLAUDE_CODE_SHELL", "/var/folders/x/T/loka-cmd-hub-1/bash"),
            ("GIT_CONFIG_COUNT", "1"),
            ("GIT_CONFIG_KEY_0", "branch.autoSetupMerge"),
            ("GIT_CONFIG_VALUE_0", "false"),
            ("GIT_CONFIG_PARAMETERS", "'safe.directory=*'"),
            ("GIT_DIR", "/elsewhere/.git"),
            ("GIT_WORK_TREE", "/elsewhere"),
            ("GIT_TEMPLATE_DIR", ""),
            ("GIT_EDITOR", "true"),
            ("CLAUDE_CODE_ENTRYPOINT", "sdk-cli"),
            ("CLAUDE_CODE_SESSION_ID", "s"),
            ("CARGO_TARGET_DIR", "/var/folders/x/T/loka-q-0123456789ab/cargo-target"),
            ("CARGO_INCREMENTAL", "0"),
            ("CARGO_PROFILE_DEV_DEBUG", "0"),
            ("CARGO_HOME", "/Users/someone/.cargo"),
            ("CARGO_MANIFEST_DIR", "/src/agent"),
            ("LOKA_CMD_HUB", "/var/folders/x/T/loka-cmd-hub-1/hub.sock"),
            ("LOKA_REAL_SHELL", "/bin/zsh"),
            ("LOKA_APPROVED_ACTIONS", "[]"),
            ("WORLD_BROWSER_LAUNCHER", "/runner/loka-agent browser"),
            ("KARNA_MODE", "world"),
        ]
        .into_iter()
        .map(|(name, value)| (name.into(), value.into()))
        .collect()
    }

    fn applied(vars: Vec<(OsString, OsString)>) -> std::collections::BTreeMap<String, String> {
        let mut env: std::collections::BTreeMap<String, String> =
            vars.iter().map(|(name, value)| (name.to_string_lossy().into_owned(), value.to_string_lossy().into_owned())).collect();
        for (name, value) in changes(vars) {
            match value {
                Some(value) => env.insert(name.to_string_lossy().into_owned(), value.to_string_lossy().into_owned()),
                None => env.remove(&*name.to_string_lossy()),
            };
        }
        env
    }

    #[test]
    fn quest_run_variables_are_stripped_and_host_ones_kept() {
        let env = applied(quest_like());
        let kept: Vec<&str> = env.keys().map(String::as_str).collect();
        assert_eq!(kept, ["CARGO_HOME", "CARGO_MANIFEST_DIR", "HOME", "PATH", "SHELL", "TMPDIR"]);
        assert_eq!(env["SHELL"], "/bin/zsh");
        assert_eq!(env["TMPDIR"], "/var/folders/x/T/loka-q-0123456789ab", "tests use the quest's own temp dir, never the shared one");
    }

    #[test]
    fn host_logins_and_git_settings_stay_when_clis_get_them() {
        let host = [
            ("CLAUDE_CODE_OAUTH_TOKEN", "sk-ant-oat01-x"),
            ("CLAUDE_CODE_USE_BEDROCK", "1"),
            ("CLAUDE_CODE_USE_VERTEX", "1"),
            ("GIT_SSH_COMMAND", "ssh -i key"),
            ("GIT_AUTHOR_NAME", "Someone"),
            ("GIT_AUTHOR_EMAIL", "someone@example.com"),
            ("GIT_CONFIG_GLOBAL", "/runner/gitconfig"),
        ];
        let env = applied(host.iter().map(|(name, value)| ((*name).into(), (*value).into())).collect());
        for (name, _) in host {
            assert_eq!(env.contains_key(name), crate::passed_on_to_clis(name), "{name}");
        }
    }

    #[test]
    fn a_claude_temp_dir_outside_a_quest_leaves_tmpdir_alone() {
        let env = applied(vec![("TMPDIR".into(), "/tmp/claude-501".into()), ("CLAUDE_CODE_TMPDIR".into(), "/Users/me/tmp/claude-501".into())]);
        assert_eq!(env.into_iter().collect::<Vec<_>>(), [("TMPDIR".to_string(), "/tmp/claude-501".to_string())]);
    }

    #[test]
    fn a_shell_without_a_command_hub_stays_as_it_is() {
        let env = applied(vec![("SHELL".into(), "/bin/bash".into()), ("GIT_DIR".into(), "/x".into())]);
        assert_eq!(env.into_iter().collect::<Vec<_>>(), [("SHELL".to_string(), "/bin/bash".to_string())]);
    }

    #[test]
    fn the_test_process_starts_without_quest_run_variables() {
        let started_with = STARTED_WITH.get().expect("the clean start ran before the tests");
        let leaked: Vec<&OsString> = started_with.iter().filter(|name| name.to_str().is_some_and(from_quest_run)).collect();
        assert!(leaked.is_empty(), "{leaked:?}");
    }
}
