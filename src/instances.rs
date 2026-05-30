use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

#[derive(Clone, Debug)]
pub struct ClaudeInstance {
    pub pid: u32,
    pub cwd: String,
    pub active: bool, // true = Claude is generating (user waiting)
}

/// Convert a CWD path to the ~/.claude/projects/ directory name.
/// e.g. "C:\\Projects\\ClaudeWatch" → "C--Projects-ClaudeWatch"
fn cwd_to_project_dir(cwd: &str) -> String {
    cwd.trim_end_matches(['\\', '/'])
        .replace(':', "-")
        .replace(['\\', '/'], "-")
}

pub fn has_activity_since(since: std::time::SystemTime) -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    let projects_dir = home.join(".claude").join("projects");
    let Ok(projects) = std::fs::read_dir(&projects_dir) else {
        return false;
    };

    for project in projects.flatten() {
        let Ok(entries) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            if let Ok(meta) = path.metadata() {
                if let Ok(modified) = meta.modified() {
                    if modified > since {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn is_session_active(cwd: &str, threshold_secs: u64) -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    let project_dir = home
        .join(".claude")
        .join("projects")
        .join(cwd_to_project_dir(cwd));
    let Ok(entries) = std::fs::read_dir(&project_dir) else {
        return false;
    };

    let now = std::time::SystemTime::now();
    let mut newest_mod = None;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        if let Ok(meta) = path.metadata() {
            if let Ok(modified) = meta.modified() {
                if newest_mod.map_or(true, |prev| modified > prev) {
                    newest_mod = Some(modified);
                }
            }
        }
    }

    newest_mod.map_or(false, |t| {
        now.duration_since(t)
            .map_or(false, |d| d.as_secs() < threshold_secs)
    })
}

pub fn detect_claude_instances() -> Vec<ClaudeInstance> {
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_exe(UpdateKind::Always)
            .with_cwd(UpdateKind::Always),
    );

    let mut instances = Vec::new();
    for (pid, process) in sys.processes() {
        let exe_path = process
            .exe()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        // Native Windows installation: ~/.local/bin/claude.exe
        let is_claude_code = exe_path.contains(".local") && exe_path.ends_with("claude.exe");
        if !is_claude_code {
            continue;
        }

        let cwd = process
            .cwd()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        let active = is_session_active(&cwd, 15);

        instances.push(ClaudeInstance {
            pid: pid.as_u32(),
            cwd,
            active,
        });
    }
    instances
}
