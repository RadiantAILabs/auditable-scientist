//! Author-selected, advisory tests within an existing role session.
use polaris::{
    system::{
        plugin::{Plugin, PluginId, Version},
        server::Server,
    },
    tools::{ToolError, ToolRegistry, ToolsPlugin, toolset},
};
use polaris_code::tools::CallRoot;
use polaris_cosmos::flow::AgentRequest;
use polaris_scientist::{config::ScientistConfig, durable::copy_snapshot, eval, phases::task};
use serde_json::json;
use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

pub(super) const TOOL: &str = "test_candidate";
const MAX_TESTS: u32 = 1;

struct TestCall {
    root: PathBuf,
    config: ScientistConfig,
    role: String,
    used: AtomicU32,
    active: tokio::sync::Mutex<()>,
}

/// Capabilities keyed by the unforgeable tool invocation's call root.
#[derive(Clone, Default)]
pub(super) struct AuthorTests(Arc<Mutex<BTreeMap<PathBuf, Arc<TestCall>>>>);

/// Removing the call's capability on cancellation also prevents later use.
pub(super) struct TestLease(AuthorTests, PathBuf);
impl Drop for TestLease {
    fn drop(&mut self) {
        if let Ok(mut calls) = self.0.0.lock() {
            calls.remove(&self.1);
        }
    }
}

impl AuthorTests {
    pub(super) fn enable(
        &self,
        request: &AgentRequest,
        folder: &Path,
        root: &Path,
        config: Option<&ScientistConfig>,
    ) -> Option<TestLease> {
        if !request.grants.edit
            || !matches!(
                request.role.as_str(),
                "program_author" | "variant_author" | "variant_retrofit"
            )
        {
            return None;
        }
        let config = config?.clone();
        self.0.lock().ok()?.insert(
            folder.into(),
            Arc::new(TestCall {
                root: root.into(),
                config,
                role: request.role.clone(),
                used: AtomicU32::new(0),
                active: tokio::sync::Mutex::new(()),
            }),
        );
        Some(TestLease(self.clone(), folder.into()))
    }
}

fn failure(error: impl std::fmt::Display) -> ToolError {
    ToolError::execution_error(error.to_string())
}

#[toolset]
impl AuthorTests {
    /// Optionally run your candidate through the sealed task evaluator, read
    /// the advisory metrics and correctness, then continue editing or finish.
    /// At most one test per author call. Official scoring runs independently.
    #[tool]
    async fn test_candidate(
        &self,
        /// Candidate directory under outputs/, such as outputs/program_1.
        program_dir: String,
        /// Time limit in seconds, capped at the task evaluator's limit.
        timeout_seconds: Option<u64>,
        #[context] call_root: Option<CallRoot>,
    ) -> Result<String, ToolError> {
        let folder = call_root
            .ok_or_else(|| failure("missing author call root"))?
            .path()
            .to_path_buf();
        let call = self
            .0
            .lock()
            .map_err(failure)?
            .get(&folder)
            .cloned()
            .ok_or_else(|| failure("candidate testing is not granted to this role call"))?;
        let _active = call.active.lock().await;
        let parts = Path::new(&program_dir).components().collect::<Vec<_>>();
        if parts.len() != 2 || parts[0] != Component::Normal("outputs".as_ref()) {
            return Err(failure("test candidate must be outputs/program_<index>"));
        }
        let index = parts[1]
            .as_os_str()
            .to_str()
            .and_then(|s| s.strip_prefix("program_"))
            .and_then(|s| s.parse::<u32>().ok())
            .ok_or_else(|| failure("invalid candidate index"))?;
        let allowed = match call.role.as_str() {
            "program_author" => index == 1,
            "variant_retrofit" => index == 0,
            _ => {
                index > 0
                    && u64::from(index)
                        <= call.config.u64("ablations.execution.max_evals_per_variant")
            }
        };
        if !allowed {
            return Err(failure("candidate index is outside this role's contract"));
        }
        let source = folder.join(&program_dir);
        // The fingerprint walk rejects every symlink, including nested files.
        if std::fs::symlink_metadata(&source)
            .map_err(failure)?
            .file_type()
            .is_symlink()
            || !source
                .canonicalize()
                .map_err(failure)?
                .starts_with(folder.join("outputs"))
        {
            return Err(failure("candidate escapes author outputs"));
        }
        eval::fingerprint(&source).map_err(failure)?;
        let timeout = timeout_seconds
            .unwrap_or(call.config.u64("evaluation.timeout_seconds"))
            .min(call.config.u64("evaluation.timeout_seconds"));
        if timeout == 0 {
            return Err(failure("test timeout must be positive"));
        }
        let used = call
            .used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MAX_TESTS).then_some(n + 1)
            })
            .map_err(|_| failure("author test limit reached (1 per call)"))?
            + 1;
        let scratch = folder.join("tests").join(format!("{used:03}"));
        // Fresh host-owned paths, never a symlink or a caller-created directory.
        match std::fs::create_dir(folder.join("tests")) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && used > 1 => {}
            Err(e) => return Err(failure(e)),
        }
        let tests = folder.join("tests");
        if std::fs::symlink_metadata(&tests)
            .map_err(failure)?
            .file_type()
            .is_symlink()
            || tests.canonicalize().map_err(failure)? != tests
        {
            return Err(failure("test folder escapes author workspace"));
        }
        std::fs::create_dir(&scratch).map_err(failure)?;
        let candidate = scratch.join("candidate");
        copy_snapshot(&source, &candidate, false).map_err(failure)?;
        polaris_scientist::phases::ablations::artifacts::remove_results(&candidate)
            .map_err(failure)?;
        let results = scratch.join("results");
        let mut request =
            task::evaluator_request(&call.root, &call.config, candidate.clone(), results.clone())
                .map_err(failure)?;
        request.timeout = Duration::from_secs(timeout);
        if call.role != "program_author" {
            request.environment.insert(
                "NUM_REPLICATIONS".into(),
                call.config
                    .u64("ablations.execution.num_replications")
                    .to_string(),
            );
            let spec = folder.join("inputs/variant_spec.json");
            if spec.is_file() {
                let spec: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(spec).map_err(failure)?)
                        .map_err(failure)?;
                request.environment.insert(
                    "EVALUATION_ID".into(),
                    format!(
                        "{}_advisory_{used}",
                        spec["variant_id"].as_str().unwrap_or("variant")
                    ),
                );
                for (key, field) in [
                    ("VARIANT_ID", "variant_id"),
                    ("VARIANT_KIND", "kind"),
                    ("BASE_PROGRAM", "base_program"),
                ] {
                    if let Some(value) = spec[field].as_str() {
                        request.environment.insert(key.into(), value.into());
                    }
                }
            }
        }
        let result = eval::test_evaluator(request, &scratch).await;
        let response = match result {
            Ok(result) => {
                let mut response = json!({"advisory":true,"test":used,"tests_remaining":MAX_TESTS-used,"metrics":result.metrics,"correctness":result.correctness,"results_dir":results,"integrity_valid":result.manifest.integrity_valid});
                if call.role != "program_author" {
                    response["analysis_checks"] =
                        match polaris_scientist::phases::ablations::contracts::mechanical(
                            &candidate, &result,
                        ) {
                            Ok(checks) => json!(checks),
                            Err(error) => json!({"error":error.to_string()}),
                        };
                }
                response
            }
            Err(error) => {
                json!({"advisory":true,"test":used,"tests_remaining":MAX_TESTS-used,"error":error.to_string(),"results_dir":results})
            }
        };
        std::fs::write(
            scratch.join("report.json"),
            serde_json::to_vec_pretty(&response).map_err(failure)?,
        )
        .map_err(failure)?;
        Ok(response.to_string())
    }
}

/// Registers the test tool; a live author capability is required at dispatch.
pub(super) struct AuthorTestsPlugin(pub AuthorTests);
impl Plugin for AuthorTestsPlugin {
    const ID: &'static str = "polaris_scientist::author_tests";
    const VERSION: Version = Version::new(0, 1, 0);
    fn dependencies(&self) -> Vec<PluginId> {
        vec![PluginId::of::<ToolsPlugin>()]
    }
    fn build(&self, server: &mut Server) {
        server
            .get_resource_mut::<ToolRegistry>()
            .expect("ToolsPlugin builds first")
            .register_toolset(self.0.clone());
    }
}
