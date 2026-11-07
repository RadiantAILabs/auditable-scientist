//! Role fragments merge by unique key; configuration overrides apply last.
use crate::config::{ConfigError, ScientistConfig};
pub use polaris_cosmos::flow::{Role, RoleBook};
use std::path::Path;
/// Loads role fragments and applies validated per-role overrides.
pub fn load(dir: &Path, cfg: &ScientistConfig) -> Result<RoleBook, ConfigError> {
    let mut book = RoleBook::load(dir)?;
    for (role, key) in [
        ("ideation", "investigation.execution.idea_timeout_seconds"),
        (
            "program_author",
            "investigation.execution.run_timeout_seconds",
        ),
        (
            "variant_retrofit",
            "ablations.execution.retrofit_timeout_seconds",
        ),
        (
            "variant_propose",
            "ablations.execution.generation_timeout_seconds",
        ),
        (
            "variant_select",
            "ablations.execution.selection_timeout_seconds",
        ),
        (
            "variant_author",
            "ablations.execution.variant_timeout_seconds",
        ),
        (
            "variant_summarizer",
            "ablations.execution.readme_timeout_seconds",
        ),
        (
            "posthoc_proposer",
            "posthoc.execution.generation_timeout_seconds",
        ),
        (
            "posthoc_selector",
            "posthoc.execution.selection_timeout_seconds",
        ),
        (
            "posthoc_analyst",
            "posthoc.execution.analysis_timeout_seconds",
        ),
    ] {
        if let Some(role) = book.roles.get_mut(role) {
            role.timeout_seconds = cfg.u64(key);
        }
    }
    if let Some(overrides) = cfg.value("roles").and_then(toml::Value::as_table) {
        for (key, fields) in overrides {
            let role = book
                .roles
                .get_mut(key)
                .ok_or_else(|| format!("unknown role override {key}"))?;
            let mut value = toml::Value::try_from(&*role)?;
            for (field, override_value) in fields.as_table().ok_or("invalid role override")? {
                value
                    .as_table_mut()
                    .unwrap()
                    .insert(field.clone(), override_value.clone());
            }
            *role = value.try_into().map_err(|e| format!("role {key}: {e}"))?;
        }
    }
    for (key, role) in &book.roles {
        if role.max_turns == 0 || role.timeout_seconds == 0 {
            return Err(format!("role {key}: turns and timeout must be positive").into());
        }
        if role.max_output_tokens == Some(0) {
            return Err(format!("role {key}: max_output_tokens must be positive when set").into());
        }
    }
    Ok(book)
}
