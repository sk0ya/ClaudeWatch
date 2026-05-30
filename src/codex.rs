use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct CodexWindowInfo {
    pub used_percent: f64,
    pub resets_at: u64, // Unix timestamp (seconds)
}

#[derive(Clone, Debug)]
pub struct CodexRateLimit {
    pub limit_id: String,
    pub limit_name: Option<String>,
    pub primary: CodexWindowInfo,
    pub secondary: CodexWindowInfo,
}

#[derive(Clone, Debug, Default)]
pub struct CodexRateLimitState {
    pub limits: Vec<CodexRateLimit>,
    pub error: Option<String>,
}

pub fn fetch_codex_rate_limits() -> CodexRateLimitState {
    let result = (|| -> Result<Vec<CodexRateLimit>, String> {
        let codex_dir = dirs::home_dir()
            .ok_or_else(|| "No home dir".to_string())?
            .join(".codex")
            .join("sessions");

        if !codex_dir.exists() {
            return Err("Codex not installed".into());
        }

        // Collect all JSONL session files with modification times
        let mut files: Vec<(std::path::PathBuf, std::time::SystemTime)> = Vec::new();
        let Ok(year_iter) = std::fs::read_dir(&codex_dir) else {
            return Err("Cannot read sessions".into());
        };
        for ye in year_iter.flatten() {
            let Ok(mi) = std::fs::read_dir(ye.path()) else {
                continue;
            };
            for me in mi.flatten() {
                let Ok(di) = std::fs::read_dir(me.path()) else {
                    continue;
                };
                for de in di.flatten() {
                    // de = day directory; read files inside it
                    let Ok(fi) = std::fs::read_dir(de.path()) else {
                        continue;
                    };
                    for fe in fi.flatten() {
                        let p = fe.path();
                        if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                            if let Ok(meta) = p.metadata() {
                                if let Ok(m) = meta.modified() {
                                    files.push((p, m));
                                }
                            }
                        }
                    }
                }
            }
        }

        // Newest files first
        files.sort_by(|a, b| b.1.cmp(&a.1));

        // Read up to 5 most recent files; track latest rate_limits per limit_id
        let mut latest: HashMap<String, (CodexRateLimit, String)> = HashMap::new();
        for (path, _) in files.iter().take(5) {
            let Ok(content) = std::fs::read_to_string(path) else {
                continue;
            };
            for line in content.lines() {
                let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                if val["type"].as_str() != Some("event_msg") {
                    continue;
                }
                let payload = &val["payload"];
                if payload["type"].as_str() != Some("token_count") {
                    continue;
                }
                let rl = &payload["rate_limits"];
                if rl.is_null() {
                    continue;
                }
                let Some(limit_id) = rl["limit_id"].as_str() else {
                    continue;
                };
                let (Some(prim), Some(sec)) = (rl.get("primary"), rl.get("secondary")) else {
                    continue;
                };

                let ts = val["timestamp"].as_str().unwrap_or("").to_string();
                let entry = CodexRateLimit {
                    limit_id: limit_id.to_string(),
                    limit_name: rl["limit_name"].as_str().map(|s| s.to_string()),
                    primary: CodexWindowInfo {
                        used_percent: prim["used_percent"].as_f64().unwrap_or(0.0),
                        resets_at: prim["resets_at"].as_u64().unwrap_or(0),
                    },
                    secondary: CodexWindowInfo {
                        used_percent: sec["used_percent"].as_f64().unwrap_or(0.0),
                        resets_at: sec["resets_at"].as_u64().unwrap_or(0),
                    },
                };
                latest
                    .entry(limit_id.to_string())
                    .and_modify(|e| {
                        if ts > e.1 {
                            *e = (entry.clone(), ts.clone());
                        }
                    })
                    .or_insert((entry, ts));
            }
        }

        let mut limits: Vec<CodexRateLimit> = latest.into_values().map(|(e, _)| e).collect();
        limits.sort_by(|a, b| a.limit_id.cmp(&b.limit_id));
        Ok(limits)
    })();

    match result {
        Ok(limits) if !limits.is_empty() => CodexRateLimitState {
            limits,
            error: None,
        },
        Ok(_) => CodexRateLimitState {
            limits: vec![],
            error: Some("No Codex data".into()),
        },
        Err(e) => CodexRateLimitState {
            limits: vec![],
            error: Some(e),
        },
    }
}
