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
