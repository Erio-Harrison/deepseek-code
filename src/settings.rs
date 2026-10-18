//! Sandbox grants: seeded from the user's settings file, widened at runtime by
//! approved escalations, never written back.
//!
//! The file is `~/.config/deepseek-code/settings.json` (or under
//! `$XDG_CONFIG_HOME`), not in the workspace: everything in it widens the
//! sandbox, and the workspace is where the sandboxed agent can write.
//!
//! ```json
//! {
//!   "sandbox": { "allowWrite": ["~/.cache/build"] },
//!   "projects": {
//!     "~/src/thing": { "sandbox": { "allowUnsandboxedBash": true } }
//!   }
//! }
//! ```

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default)]
pub struct SandboxGrants {
    /// Writable in addition to the workspace.
    pub extra_writable: HashSet<PathBuf>,
    /// `bash` runs outside the sandbox for the rest of the session.
    pub unsandboxed_bash: bool,
}

#[derive(Debug, Default, serde::Deserialize)]
struct SettingsFile {
    #[serde(default)]
    sandbox: SandboxSettings,
    #[serde(default)]
    projects: HashMap<String, ProjectSettings>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct ProjectSettings {
    #[serde(default)]
    sandbox: SandboxSettings,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SandboxSettings {
    #[serde(default)]
    allow_write: Vec<String>,
    #[serde(default)]
    allow_unsandboxed_bash: bool,
}

pub fn settings_path() -> Option<PathBuf> {
    let config_home = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
    };
    Some(config_home.join("deepseek-code").join("settings.json"))
}

/// A missing or malformed file yields empty grants rather than a failed start.
pub fn load_initial_grants(workspace: &Path) -> SandboxGrants {
    let legacy = workspace.join(".deepseek-code").join("settings.json");
    if legacy.exists() {
        tracing::warn!(
            path = %legacy.display(),
            "project-level settings files are not read any more; move its contents to the \
             user settings file under a `projects` entry for this workspace"
        );
    }
    match settings_path() {
        Some(path) => load_grants_from(&path, workspace),
        None => SandboxGrants::default(),
    }
}

pub fn load_grants_from(path: &Path, workspace: &Path) -> SandboxGrants {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return SandboxGrants::default(),
        Err(err) => {
            tracing::warn!(%err, path = %path.display(), "failed to read settings file, ignoring");
            return SandboxGrants::default();
        }
    };

    let parsed: SettingsFile = match serde_json::from_str(&text) {
        Ok(parsed) => parsed,
        Err(err) => {
            tracing::warn!(%err, path = %path.display(), "failed to parse settings file, ignoring");
            return SandboxGrants::default();
        }
    };

    let mut sections = vec![&parsed.sandbox];
    let workspace = canonical(workspace);
    for (key, project) in &parsed.projects {
        if canonical(&expand_tilde(key)) == workspace {
            sections.push(&project.sandbox);
        }
    }

    let mut grants = SandboxGrants::default();
    for section in sections {
        grants
            .extra_writable
            .extend(section.allow_write.iter().map(|raw| expand_tilde(raw)));
        grants.unsandboxed_bash |= section.allow_unsandboxed_bash;
    }

    for path in &grants.extra_writable {
        if !path.is_dir() {
            tracing::warn!(path = %path.display(), "allowWrite entry is not an existing directory, it will have no effect until it is");
        }
    }
    grants
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn expand_tilde(raw: &str) -> PathBuf {
    match raw.strip_prefix("~/") {
        Some(rest) => match std::env::var("HOME") {
            Ok(home) => PathBuf::from(home).join(rest),
            Err(_) => PathBuf::from(raw),
        },
        None => PathBuf::from(raw),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        dir: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "deepseek_code_settings_{name}_{}",
                std::process::id()
            ));
            std::fs::create_dir_all(dir.join("ws")).unwrap();
            std::fs::create_dir_all(dir.join("other")).unwrap();
            Self { dir }
        }
        fn settings(&self, json: &str) -> PathBuf {
            let path = self.dir.join("settings.json");
            std::fs::write(&path, json).unwrap();
            path
        }
        fn ws(&self) -> PathBuf {
            self.dir.join("ws")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn missing_file_yields_empty_grants() {
        let f = Fixture::new("missing");
        let grants = load_grants_from(&f.dir.join("nope.json"), &f.ws());
        assert!(grants.extra_writable.is_empty());
        assert!(!grants.unsandboxed_bash);
    }

    #[test]
    fn global_section_applies_and_expands_tilde() {
        let f = Fixture::new("global");
        let path = f.settings(
            r#"{"sandbox":{"allowWrite":["~/.kube","/tmp/build"],"allowUnsandboxedBash":true}}"#,
        );
        let grants = load_grants_from(&path, &f.ws());
        assert!(grants.unsandboxed_bash);
        assert!(grants.extra_writable.contains(&PathBuf::from("/tmp/build")));
        let home = std::env::var("HOME").unwrap();
        assert!(
            grants
                .extra_writable
                .contains(&PathBuf::from(home).join(".kube"))
        );
    }

    #[test]
    fn project_section_matches_this_workspace_only_and_adds_to_global() {
        let f = Fixture::new("project");
        let ws = f.ws();
        // The key is spelled with a `..` detour to check it's compared resolved.
        let key = f.dir.join("other").join("..").join("ws");
        let path = f.settings(&format!(
            r#"{{"sandbox":{{"allowWrite":["/tmp/global"]}},
                "projects":{{
                  "{}":{{"sandbox":{{"allowWrite":["/tmp/mine"],"allowUnsandboxedBash":true}}}},
                  "{}":{{"sandbox":{{"allowWrite":["/tmp/theirs"],"allowUnsandboxedBash":true}}}}
                }}}}"#,
            key.display(),
            f.dir.join("other").display()
        ));

        let grants = load_grants_from(&path, &ws);
        assert!(grants.unsandboxed_bash);
        assert!(grants.extra_writable.contains(&PathBuf::from("/tmp/global")));
        assert!(grants.extra_writable.contains(&PathBuf::from("/tmp/mine")));
        assert!(!grants.extra_writable.contains(&PathBuf::from("/tmp/theirs")));

        let elsewhere = load_grants_from(&path, &f.dir.join("other"));
        assert!(elsewhere.unsandboxed_bash);
        assert!(!elsewhere.extra_writable.contains(&PathBuf::from("/tmp/mine")));
    }

    #[test]
    fn malformed_json_yields_empty_grants_not_a_panic() {
        let f = Fixture::new("bad");
        let path = f.settings("not json");
        let grants = load_grants_from(&path, &f.ws());
        assert!(grants.extra_writable.is_empty());
        assert!(!grants.unsandboxed_bash);
    }

    #[test]
    fn project_level_file_in_the_workspace_is_ignored() {
        let f = Fixture::new("legacy");
        let ws = f.ws();
        std::fs::create_dir_all(ws.join(".deepseek-code")).unwrap();
        std::fs::write(
            ws.join(".deepseek-code/settings.json"),
            r#"{"sandbox":{"allowUnsandboxedBash":true,"allowWrite":["/"]}}"#,
        )
        .unwrap();
        let grants = load_grants_from(&f.dir.join("nope.json"), &ws);
        assert!(!grants.unsandboxed_bash);
        assert!(grants.extra_writable.is_empty());
    }

    #[test]
    fn settings_path_follows_xdg_config_home_then_home() {
        unsafe { std::env::set_var("XDG_CONFIG_HOME", "/xdg") };
        assert_eq!(
            settings_path(),
            Some(PathBuf::from("/xdg/deepseek-code/settings.json"))
        );
        unsafe { std::env::remove_var("XDG_CONFIG_HOME") };
        let home = std::env::var("HOME").unwrap();
        assert_eq!(
            settings_path(),
            Some(PathBuf::from(home).join(".config/deepseek-code/settings.json"))
        );
    }
}
