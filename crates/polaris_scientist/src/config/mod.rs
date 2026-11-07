//! Strict, ordered configuration resolution. Monetary literals retain decimal text
//! until converted directly to integer millionths of a dollar.
use polaris::system::resource::GlobalResource;
use polaris_cosmos::flow::{Micros, dollars, parse_micros};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    fs,
    path::{Path, PathBuf},
};
/// Validated configuration document using the public key vocabulary.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ScientistConfig {
    /// Fully resolved values in the public dotted-key vocabulary.
    pub values: toml::Table,
}
impl GlobalResource for ScientistConfig {}
impl Default for ScientistConfig {
    fn default() -> Self {
        Self {
            values: toml::from_str(include_str!("defaults.toml"))
                .expect("valid built-in configuration"),
        }
    }
}
/// Dedicated command-line overrides, applied after files and assignments.
#[derive(Clone, Debug, Default)]
pub struct ConfigFlags {
    /// Exact run ceiling.
    pub max_usd: Option<Micros>,
    /// Results envelope directory.
    pub results_root: Option<PathBuf>,
    /// Optional fixed run identifier.
    pub run_id: Option<String>,
    /// Task source directory.
    pub task_dir: Option<PathBuf>,
}
/// Invalid configuration input.
pub type ConfigError = Box<dyn std::error::Error + Send + Sync>;
/// Fixed original phase ordering used for selection and reset.
pub const PHASES: [&str; 6] = [
    "task_drafting",
    "investigation",
    "ablations",
    "final_evaluation",
    "posthoc",
    "paper",
];
impl ScientistConfig {
    /// Resolves defaults, files, assignments and dedicated flags in order.
    pub fn resolve(
        files: &[PathBuf],
        sets: &[String],
        flags: ConfigFlags,
    ) -> Result<Self, ConfigError> {
        let mut cfg = Self::default();
        for file in files {
            let text = preserve_money(&fs::read_to_string(file)?);
            let table: toml::Table = toml::from_str(&text)?;
            merge(&mut cfg.values, table, "")?;
        }
        for set in sets {
            let (key, value) = set.split_once('=').ok_or("expected key=value")?;
            let text = if key.ends_with("max_usd") {
                format!("value = \"{}\"", value.trim_matches('"'))
            } else {
                format!("value = {value}")
            };
            let parsed: toml::Table = toml::from_str(&text)?;
            cfg.set(key, parsed["value"].clone())?;
        }
        if let Some(v) = flags.max_usd {
            cfg.set("budget.max_usd", toml::Value::String(dollars(v)))?;
        }
        if let Some(v) = flags.results_root {
            cfg.set(
                "paths.results_root",
                toml::Value::String(v.display().to_string()),
            )?;
        }
        if let Some(v) = flags.run_id {
            cfg.set("run_id", toml::Value::String(v))?;
        }
        if let Some(v) = flags.task_dir {
            cfg.set("task.dir", toml::Value::String(v.display().to_string()))?;
        }
        cfg.validate()?;
        Ok(cfg)
    }
    /// Returns a raw resolved value by dotted key.
    pub fn value(&self, key: &str) -> Option<&toml::Value> {
        let mut parts = key.split('.');
        let mut value = self.values.get(parts.next()?)?;
        for part in parts {
            value = value.get(part)?;
        }
        Some(value)
    }
    /// Deserializes a resolved value to its caller-owned type.
    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Result<T, ConfigError> {
        self.value(key)
            .ok_or_else(|| format!("missing configuration key {key}").into())
            .and_then(|v| v.clone().try_into().map_err(Into::into))
    }
    /// Gets a validated unsigned integer.
    pub fn u64(&self, key: &str) -> u64 {
        self.value(key)
            .and_then(toml::Value::as_integer)
            .expect("validated integer") as u64
    }
    /// Gets a validated boolean.
    pub fn bool(&self, key: &str) -> bool {
        self.value(key)
            .and_then(toml::Value::as_bool)
            .expect("validated boolean")
    }
    /// Gets a validated string.
    pub fn string(&self, key: &str) -> &str {
        self.value(key)
            .and_then(toml::Value::as_str)
            .expect("validated string")
    }
    /// Gets a directory or file path.
    pub fn path(&self, key: &str) -> PathBuf {
        PathBuf::from(self.string(key))
    }
    /// Gets an exact dollar value as micro-dollars.
    pub fn micros(&self, key: &str) -> Result<Micros, ConfigError> {
        let value = self.value(key).ok_or("missing money key")?;
        Ok(parse_micros(&decimal(value))?)
    }
    /// Gets a fraction as an integer out of one million.
    pub fn fraction(&self, key: &str) -> Result<u64, ConfigError> {
        self.micros(key)
    }
    /// Applies a known key, rejecting unknown fields and mismatched value types.
    pub fn set(&mut self, key: &str, value: toml::Value) -> Result<(), ConfigError> {
        let parts = key.split('.').collect::<Vec<_>>();
        set_at(&mut self.values, &parts, value, key)
    }
    /// Writes the full resolved TOML document.
    pub fn write(&self, path: &Path) -> Result<(), ConfigError> {
        crate::durable::atomic_write(path, toml::to_string_pretty(&self.values)?.as_bytes())?;
        Ok(())
    }
    /// Refuses changed graph shape and a lowered global cycle cap on resume.
    pub fn compatible_resume(&self, new: &Self) -> Result<(), ConfigError> {
        for key in [
            "campaign.phases",
            "task.drafting",
            "investigation.execution.workers",
            "investigation.execution.rubric_judge",
            "review.mode",
            "runner.kind",
        ] {
            if self.value(key) != new.value(key) {
                return Err(format!("cannot change shape key {key} on resume").into());
            }
        }
        let key = "investigation.execution.prune_every_n_cycles";
        if (self.u64(key) == 0) != (new.u64(key) == 0) {
            return Err(format!("cannot change shape key {key} on resume").into());
        }
        if new.u64("investigation.execution.max_idea_cycles")
            < self.u64("investigation.execution.max_idea_cycles")
        {
            return Err("cannot lower investigation.execution.max_idea_cycles on resume".into());
        }
        Ok(())
    }
    fn validate(&self) -> Result<(), ConfigError> {
        if !["session", "scripted"].contains(&self.string("runner.kind")) {
            return Err("runner.kind must be session or scripted".into());
        }
        if self.string("runner.kind") == "scripted"
            && std::env::var("POLARIS_SCIENTIST_OFFLINE").as_deref() != Ok("1")
        {
            return Err("runner.kind=scripted requires POLARIS_SCIENTIST_OFFLINE=1".into());
        }
        if !["best_available", "skip"].contains(&self.string("campaign.no_winner_policy")) {
            return Err("invalid campaign.no_winner_policy".into());
        }
        if !["off", "adapter"].contains(&self.string("review.mode")) {
            return Err("invalid review.mode".into());
        }
        if self.string("runner.provider") != "bedrock"
            || self.string("runner.model") != "aws.bedrock/global.anthropic.claude-sonnet-5-5"
        {
            return Err("only Bedrock Sonnet 5.5 is supported".into());
        }
        if self.u64("investigation.execution.workers") == 0 {
            return Err("investigation.execution.workers must be positive".into());
        }
        let phases = self.get::<Vec<String>>("campaign.phases")?;
        let mut previous = None;
        for phase in phases {
            let index = PHASES
                .iter()
                .position(|p| *p == phase)
                .ok_or_else(|| format!("unknown phase {phase}"))?;
            if previous.is_some_and(|p| index <= p) {
                return Err("campaign.phases must be an ordered subset without duplicates".into());
            }
            previous = Some(index);
        }
        self.micros("budget.max_usd")?;
        if self.fraction("budget.min_grant_fraction")? > 1_000_000 {
            return Err("invalid minimum grant fraction".into());
        }
        let mut previous = 0;
        for phase in PHASES {
            let fraction = self.fraction(&format!("budget.cumulative_share.{phase}"))?;
            if fraction < previous || fraction > 1_000_000 {
                return Err("phase shares must be cumulative fractions in [0,1]".into());
            }
            previous = fraction;
        }
        Ok(())
    }
}
fn decimal(value: &toml::Value) -> String {
    match value {
        toml::Value::String(v) => v.clone(),
        v => v.to_string(),
    }
}
fn set_at(
    table: &mut toml::Table,
    parts: &[&str],
    value: toml::Value,
    key: &str,
) -> Result<(), ConfigError> {
    if parts.first() == Some(&"roles") {
        if parts.len() != 3
            || ![
                "prompt",
                "node",
                "max_turns",
                "max_usd",
                "timeout_seconds",
                "max_output_tokens",
                "shell",
                "read_only",
                "skills",
            ]
            .contains(&parts[2])
        {
            return Err(format!("unknown configuration key {key}").into());
        }
        let roles = table.get_mut("roles").unwrap().as_table_mut().unwrap();
        let role = roles
            .entry(parts[1])
            .or_insert(toml::Value::Table(toml::Table::new()))
            .as_table_mut()
            .ok_or("invalid role overrides")?;
        role.insert(parts[2].into(), value);
        return Ok(());
    }
    let old = table
        .get_mut(parts[0])
        .ok_or_else(|| format!("unknown configuration key {key}"))?;
    if parts.len() > 1 {
        let sub = old
            .as_table_mut()
            .ok_or_else(|| format!("unknown configuration key {key}"))?;
        return set_at(sub, &parts[1..], value, key);
    }
    let compatible = std::mem::discriminant(old) == std::mem::discriminant(&value)
        || matches!(
            (&*old, &value),
            (toml::Value::Float(_), toml::Value::Integer(_))
        );
    if !compatible {
        return Err(format!("wrong value type for {key}").into());
    }
    if value.as_integer().is_some_and(|v| v < 0) {
        return Err(format!("negative value for {key}").into());
    }
    *old = value;
    Ok(())
}
fn merge(target: &mut toml::Table, source: toml::Table, prefix: &str) -> Result<(), ConfigError> {
    for (key, value) in source {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        if path == "roles" {
            let roles = value.as_table().ok_or("roles must be a table")?;
            for (role, fields) in roles {
                for (field, value) in fields.as_table().ok_or("role must be a table")? {
                    set_at(
                        target,
                        &["roles", role, field],
                        value.clone(),
                        &format!("roles.{role}.{field}"),
                    )?;
                }
            }
        } else if let toml::Value::Table(sub) = value {
            let dest = target
                .get_mut(&key)
                .and_then(toml::Value::as_table_mut)
                .ok_or_else(|| format!("unknown configuration key {path}"))?;
            merge(dest, sub, &path)?;
        } else {
            set_at(target, &[&key], value, &path)?;
        }
    }
    Ok(())
}
/// Quotes monetary assignments before TOML parsing to retain exact decimal digits.
pub fn preserve_money(text: &str) -> String {
    text.lines()
        .map(|line| {
            let Some((key, value)) = line.split_once('=') else {
                return line.to_string();
            };
            if key.trim() == "max_usd" {
                let value = value.split('#').next().unwrap().trim();
                if !value.starts_with('"') && !value.starts_with('\'') {
                    return format!("{key}= \"{value}\"");
                }
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}
