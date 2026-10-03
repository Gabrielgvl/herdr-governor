//! File-driven tests for `load`/`reload`/`load_credentials` — one child
//! per adapter verb. The pure DTO-layer decode cases live inline in
//! `raw.rs`'s own `mod tests`. Harness names in the fixture are made-up —
//! I9 scans strings too.

mod credentials;
mod load;
mod reload;

use std::path::{Path, PathBuf};

/// A complete, valid catalog: every required field plus all three
/// optional cap/floor tiers and every defaulted bound written.
const GOLDEN: &str = r#"
[policy]
tiers = ["fast", "standard", "frontier"]
no_change_cap = "standard"
security_floor = "frontier"
broad_change_floor = "standard"
provider_limit_threshold = 0.6
exploration_rate = 0.05
recovery_expiry_secs = 86400
cooldown_secs = 3600
max_age_secs = 86400
repair_window_secs = 900
judgment_window_secs = 1800
idle_window_secs = 900

[[catalog.operating_points]]
id = "forge-pro"
harness = "forge"
args = ["--model", "pro"]
tier = "frontier"
capabilities = ["start", "prompt_ack"]
cost_class = 3
provider = "vendor-a"

[[catalog.operating_points]]
id = "atlas-mini"
harness = "atlas"
args = ["--fast"]
tier = "fast"
capabilities = ["start"]
cost_class = 0
provider = "vendor-b"

[daemon]
herdr_socket = "/run/test/herdr.sock"
jev_base_url = "https://jev.invalid"
jev_model = "jev-test"
jev_timeout_secs = 21
herdr_op_timeout_secs = 11
agent_start_timeout_ms = 31000
reconcile_secs = 31
review_interval_secs = 301
launch_wait_secs = 61
shutdown_grace_secs = 11
retire_enabled = false
retire_grace_secs = 901
transcript_data_dirs = ["/tmp/roots-a", "/tmp/roots-b"]
transcript_project_dirs = ["/tmp/projects"]
devin_log_dir = "/tmp/native-logs"
"#;

/// Write `contents` as `dir/catalog.toml` and return its path.
fn write_catalog(dir: &Path, contents: &str) -> PathBuf {
    let path = dir.join("catalog.toml");
    std::fs::write(&path, contents).expect("fixture write succeeds");
    path
}
