"""Offline stand-in with the shipped evaluator's CLI and 300 second limit."""
import argparse
import importlib.util
import json
import multiprocessing as mp
from pathlib import Path

PROGRAM_TIMEOUT_SECONDS = 5 * 60


def worker(path, queue):
    try:
        spec = importlib.util.spec_from_file_location("candidate", path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        queue.put({"score": module.solution()})
    except Exception as error:
        queue.put({"error": str(error)})


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--program_path", default="initial.py")
    parser.add_argument("--results_dir", default="results")
    parser.add_argument("--verbose", action="store_true", default=True)
    parser.add_argument("--quiet", action="store_true")
    args = parser.parse_args()
    out = Path(args.results_dir)
    out.mkdir(parents=True, exist_ok=True)
    queue = mp.Queue()
    process = mp.Process(target=worker, args=(args.program_path, queue))
    process.start()
    process.join(PROGRAM_TIMEOUT_SECONDS)
    if process.is_alive():
        process.terminate()
        process.join(5)
        if process.is_alive():
            process.kill()
            process.join()
        result = {"error": "timeout"}
    else:
        try:
            result = queue.get(timeout=1)
        except Exception:
            result = {"error": "missing result"}
    score = result.get("score", 0)
    correct = type(score) in (float, int) and 0 <= score <= 10
    (out / "metrics.json").write_text(json.dumps({"combined_score": score}))
    (out / "correct.json").write_text(json.dumps({"correct": correct, "error": result.get("error")}))
    (out / "stdout.log").write_text("stand-in evaluated\n")
    (out / "stderr.log").write_text("")


if __name__ == "__main__":
    main()
