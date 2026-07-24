use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct CodexWindowInfo {
    pub used_percent: f64,
    pub window_minutes: u64,
    pub resets_at: u64, // Unix timestamp (seconds)
}

impl CodexWindowInfo {
    /// Short label derived from the window width, e.g. 300 -> "5h", 10080 -> "7d".
    pub fn window_label(&self) -> String {
        match self.window_minutes {
            0 => "?".into(),
            m if m % 1440 == 0 => format!("{}d", m / 1440),
            m if m % 60 == 0 => format!("{}h", m / 60),
            m => format!("{m}m"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct CodexRateLimit {
    pub limit_id: String,
    pub limit_name: Option<String>,
    pub primary: CodexWindowInfo,
    /// Codex dropped the second window in 2026-07; it is `null` in recent sessions.
    pub secondary: Option<CodexWindowInfo>,
}

fn parse_window(v: &serde_json::Value) -> Option<CodexWindowInfo> {
    Some(CodexWindowInfo {
        used_percent: v.get("used_percent")?.as_f64()?,
        window_minutes: v["window_minutes"].as_u64().unwrap_or(0),
        resets_at: v["resets_at"].as_u64().unwrap_or(0),
    })
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
                let Some(primary) = parse_window(&rl["primary"]) else {
                    continue;
                };

                let ts = val["timestamp"].as_str().unwrap_or("").to_string();
                let entry = CodexRateLimit {
                    limit_id: limit_id.to_string(),
                    limit_name: rl["limit_name"].as_str().map(|s| s.to_string()),
                    primary,
                    secondary: parse_window(&rl["secondary"]),
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

#[cfg(test)]
mod tests {
    use super::*;

    // Shape written by Codex up to 2026-07-12: primary = 5h, secondary = weekly.
    const OLD: &str = r#"{"limit_id":"codex","limit_name":null,
        "primary":{"used_percent":55.0,"window_minutes":300,"resets_at":1783857922},
        "secondary":{"used_percent":51.0,"window_minutes":10080,"resets_at":1784371149}}"#;

    // Shape written since 2026-07-14: primary = weekly, secondary dropped.
    const NEW: &str = r#"{"limit_id":"codex","limit_name":null,
        "primary":{"used_percent":2.0,"window_minutes":10080,"resets_at":1785314804},
        "secondary":null}"#;

    #[test]
    fn parses_old_two_window_shape() {
        let rl: serde_json::Value = serde_json::from_str(OLD).unwrap();
        let prim = parse_window(&rl["primary"]).unwrap();
        let sec = parse_window(&rl["secondary"]).unwrap();
        assert_eq!(prim.used_percent, 55.0);
        assert_eq!(prim.window_label(), "5h");
        assert_eq!(sec.used_percent, 51.0);
        assert_eq!(sec.window_label(), "7d");
    }

    #[test]
    fn null_secondary_is_absent_not_zero() {
        let rl: serde_json::Value = serde_json::from_str(NEW).unwrap();
        let prim = parse_window(&rl["primary"]).unwrap();
        assert_eq!(prim.used_percent, 2.0);
        assert_eq!(prim.window_label(), "7d");
        assert!(parse_window(&rl["secondary"]).is_none());
    }

    #[test]
    fn window_label_covers_odd_widths() {
        let w = |m| CodexWindowInfo { used_percent: 0.0, window_minutes: m, resets_at: 0 };
        assert_eq!(w(60).window_label(), "1h");
        assert_eq!(w(90).window_label(), "90m");
        assert_eq!(w(1440).window_label(), "1d");
        assert_eq!(w(0).window_label(), "?");
    }
}
