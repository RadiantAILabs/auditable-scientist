# Offline research fixture

`task/` is a self-contained, dependency-free task scaffold. Its evaluator accepts
`--program_path`, `--results_dir`, `--verbose`, and `--quiet`; it imports
`solution()` in a child process with a 300 second limit and writes `metrics.json`,
`correct.json`, `stdout.log`, and `stderr.log`. A numeric return in [0,10] is correct;
the seed returns 1.0. Tests can supply a candidate returning 2.0 to improve it.

This is a fast stand-in, not the shipped circle-packing scorer. It needs no
packages, credentials, network, model, or container. Campaigns use their own task's
unchanged evaluator. Copy the task to a temporary directory before running it.
`program_spec.toml` matches the shipped code-root/entrypoint schema.
