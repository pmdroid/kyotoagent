import argparse
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time
import urllib.request


SCENARIOS = {
    "lines": {
        "source": 'def count_lines(text):\n    return len(text.split("\\n"))\n',
        "fixed": 'def count_lines(text):\n    return 0 if text == "" else len(text.split("\\n"))\n',
        "check": 'from app import count_lines\nfor text, expected in [("", 0), ("one", 1), ("one\\ntwo", 2), ("one\\n", 2), ("\\n\\n", 3)]:\n    assert count_lines(text) == expected, (text, expected, count_lines(text))\n',
    },
    "terminal": {
        "source": 'def render_row(label, width):\n    return label[:width]\n',
        "check": 'from app import render_row\nfor label, width, expected in [("a", 4, "a   "), ("abcdef", 3, "abc"), ("", 2, "  "), ("abc", 0, "")]:\n    assert render_row(label, width) == expected, (label, width, render_row(label, width))\n',
    },
}
SKILLS = {
    "line-counter": (
        "Use when the user requests a fix or implementation change to document line counting, including empty input and trailing newlines.",
        "Read app.py before editing. Correct the requested counting behavior and preserve the other cases.",
    ),
    "terminal-ui": (
        "Use for changes to terminal rendering, sidebar layout, column width, padding, or truncation.",
        "Read app.py before editing. Check the behavior for short, long, and empty labels and zero width.",
    ),
    "verify-python": (
        "Use when checking Python behavior, running the project's Python tests, or verifying a Python change before reporting completion.",
        "Run python3 verify.py and inspect its exit status and output. Report the actual result. Keep the verification script unchanged.",
    ),
    "release-package": (
        "Use when the user asks to publish a package version or create a release.",
        "Read the package version and release instructions before preparing a release.",
    ),
}
WORK_TOOLS = {"read_file", "grep", "list_dir", "run", "start_task", "search_replace", "write_file"}
PASS_MARKER = "PASS: eval fixture verified"


def save_json(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def git(repo, *args):
    return subprocess.check_output(["git", "-C", str(repo), *args], text=True).strip()


def api(socket, method, path, data=None):
    command = ["curl", "--unix-socket", str(socket), "--silent", "--show-error", "--fail-with-body", "--max-time", "15", "-H", "Host: kyotoagent", "-X", method]
    if data is not None:
        command.extend(["-H", "Content-Type: application/json", "--data-binary", json.dumps(data)])
    command.append("http://kyotoagent" + path)
    result = subprocess.run(command, text=True, capture_output=True, timeout=20)
    if result.returncode:
        raise RuntimeError(f"{method} {path}: {result.stderr or result.stdout}")
    if path.endswith("/events"):
        return [json.loads(line) for line in result.stdout.splitlines() if line]
    return json.loads(result.stdout) if result.stdout.strip() else None


def build_variant(repo, output, name, revision, target):
    sha = git(repo, "rev-parse", revision + "^{commit}")
    root = output / "build" / name
    root.mkdir(parents=True)
    checkout = root / "checkout"
    print(f"Building {name} at {sha[:12]}", flush=True)
    subprocess.run(["git", "-C", str(repo), "worktree", "add", "--detach", str(checkout), sha], check=True, capture_output=True)
    try:
        with (root / "build.log").open("w") as log:
            subprocess.run(["cargo", "build", "--offline", "--bin", "kyotoagent", "--target-dir", str(target)], cwd=checkout, stdout=log, stderr=subprocess.STDOUT, check=True, timeout=900)
        binary = root / "kyotoagent"
        shutil.copy2(target / "debug" / "kyotoagent", binary)
    finally:
        subprocess.run(["git", "-C", str(repo), "worktree", "remove", "--force", str(checkout)], check=True, capture_output=True)
    return {"name": name, "commit": sha, "binary": str(binary)}


def start_server(variant, output, base, model):
    home = output / "servers" / variant["name"]
    root = home / ".kyotoagent"
    root.mkdir(parents=True)
    socket = root / "kyotoagent.sock"
    if len(str(socket).encode()) >= 104:
        raise ValueError("Use a shorter output path for the Unix sockets")
    config = f"base_url = {json.dumps(base)}\nmodel = {json.dumps(model)}\ntitle_model = \"\"\nyolo = true\n"
    (root / "config.toml").write_text(config)
    environment = dict(os.environ, HOME=str(home), KYOTOAGENT_ROOT=str(root))
    with (home / "serve.log").open("w") as log:
        process = subprocess.Popen([variant["binary"], "serve"], cwd=home, env=environment, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
    server = {"process": process, "socket": socket, "environment": environment, **variant}
    try:
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            if process.poll() is not None:
                raise RuntimeError(f"{variant['name']} serve exited; see {home / 'serve.log'}")
            if socket.exists():
                api(socket, "GET", "/v1/sessions")
                return server
            time.sleep(0.1)
        raise TimeoutError(f"{variant['name']} serve did not become ready")
    except BaseException:
        stop_server(server)
        raise


def stop_server(server):
    process = server["process"]
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)


def prepare_workspace(case, workspace):
    workspace.mkdir(parents=True)
    scenario = SCENARIOS[case["scenario"]]
    source = scenario["fixed"] if case.get("fixed") else scenario["source"]
    files = {
        "app.py": source,
        "verify.py": scenario["check"] + f"print({PASS_MARKER!r})\n",
        "README.md": "Release name: Bluebird\n",
        "user-notes.txt": "Keep this unfinished user draft.\n",
    }
    for name, (description, body) in SKILLS.items():
        disabled = "disable-model-invocation: true\n" if name in case.get("disabled", []) else ""
        files[f".agents/skills/{name}/SKILL.md"] = f"---\nname: {name}\ndescription: {description}\n{disabled}---\n\n{body}\n"
    for name, contents in files.items():
        path = workspace / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(contents)
    subprocess.run(["git", "init", "-q", str(workspace)], check=True, capture_output=True)
    subprocess.run(["git", "-C", str(workspace), "add", "."], check=True, capture_output=True)
    subprocess.run(["git", "-C", str(workspace), "-c", "user.name=Kyoto eval", "-c", "user.email=eval@localhost", "commit", "-qm", "Fixture"], check=True, capture_output=True)
    return files


def verification_call(body):
    arguments = body.get("args", {}).get("argv", [])
    return body.get("tool") in {"run", "start_task"} and any("verify.py" in argument for argument in arguments)


def score_trace(case, events):
    calls = [event["body"] for event in events if event["kind"] == "tool_call"]
    loaded = {}
    attempted = {body.get("args", {}).get("name", "") for body in calls if body["tool"] == "use_skill"}
    for index, event in enumerate(events):
        body = event["body"]
        if event["kind"] == "tool_result" and body["tool"] == "use_skill" and not body.get("is_error"):
            match = re.search(r"<name>([^<]+)</name>", body.get("output", ""))
            if match:
                loaded.setdefault(match[1], index)
    first_work = next((index for index, event in enumerate(events) if event["kind"] == "tool_call" and event["body"]["tool"] in WORK_TOOLS), len(events))
    first_verify = next((index for index, event in enumerate(events) if event["kind"] == "tool_call" and verification_call(event["body"])), len(events))
    expected = case["expected"]
    missing = sorted(set(expected) - set(loaded))
    unexpected = sorted(attempted - set(expected))
    late = sorted(name for name, phase in expected.items() if name in loaded and loaded[name] >= (first_verify if phase == "verify" else first_work))
    verified = first_verify < len(events) and any(event["kind"] == "tool_result" and not event["body"].get("is_error") and event["body"]["tool"] in {"run", "check_task"} and "exited 0" in event["body"].get("output", "") and PASS_MARKER in event["body"].get("output", "") for event in events)
    return {"skills": list(loaded), "missing": missing, "unexpected": unexpected, "late": late, "selection_pass": not (missing or unexpected or late), "verification_pass": verified, "tool_calls": len(calls)}


def run_case(server, case, repeat, output, timeout):
    root = output / "runs" / case["id"] / str(repeat) / server["name"]
    workspace = root / "workspace"
    original = prepare_workspace(case, workspace)
    save_json(root / "case.json", case)
    started = time.monotonic()
    events = []
    view = {}
    error = None
    session = None
    try:
        prompt = subprocess.run([server["binary"], "systemprompt"], cwd=workspace, env=server["environment"], capture_output=True, text=True, check=True, timeout=15)
        (root / "prompt.txt").write_text(prompt.stdout)
        session = api(server["socket"], "POST", "/v1/sessions", {"workspace": str(workspace)})["id"]
        receipt = api(server["socket"], "POST", f"/v1/sessions/{session}/messages", {"text": case["prompt"]})
        save_json(root / "message.json", receipt)
        while time.monotonic() - started < timeout:
            view = api(server["socket"], "GET", f"/v1/sessions/{session}/view")
            if view.get("status") in {"idle", "waiting"}:
                if view["status"] == "waiting":
                    error = "agent_waiting_for_input"
                break
            time.sleep(0.25)
        else:
            error = "turn_timeout"
    except (OSError, RuntimeError, subprocess.SubprocessError) as failure:
        error = f"infrastructure: {failure}"
    finally:
        if session is not None:
            try:
                if error:
                    api(server["socket"], "POST", f"/v1/sessions/{session}/cancel")
                events = api(server["socket"], "GET", f"/v1/sessions/{session}/events")
            except (OSError, RuntimeError, subprocess.SubprocessError) as failure:
                error = f"infrastructure: {failure}"
        (root / "events.jsonl").write_text("".join(json.dumps(event) + "\n" for event in events))
        save_json(root / "view.json", view)
    score = score_trace(case, events)
    protected = list(original) if case.get("readonly") else [name for name in original if name != "app.py"]
    preserved = all((workspace / name).is_file() and (workspace / name).read_bytes() == original[name].encode() for name in protected)
    result_text = "\n".join(event["body"].get("text", "") for event in events if event["kind"] == "result")
    if "answer" in case:
        outcome = re.search(r"\b" + re.escape(case["answer"]) + r"\b", result_text, re.IGNORECASE) is not None
    else:
        try:
            check = subprocess.run([sys.executable, "-c", SCENARIOS[case["scenario"]]["check"]], cwd=workspace, capture_output=True, text=True, timeout=10)
            (root / "check.txt").write_text(check.stdout + check.stderr + f"\nExit code: {check.returncode}\n")
            outcome = check.returncode == 0
        except subprocess.TimeoutExpired:
            (root / "check.txt").write_text("Verification timed out after 10 seconds\n")
            outcome = False
    diff = subprocess.run(["git", "-C", str(workspace), "diff"], capture_output=True, text=True, check=True)
    (root / "changes.diff").write_text(diff.stdout)
    score.update({"case": case["id"], "kind": case["kind"], "variant": server["name"], "repeat": repeat, "error": error, "task_pass": bool(result_text) and preserved and outcome and error is None, "preserved": preserved, "seconds": round(time.monotonic() - started, 2)})
    verification_needed = "answer" not in case
    score["pass"] = score["selection_pass"] and score["task_pass"] and (score["verification_pass"] or not verification_needed)
    save_json(root / "score.json", score)
    print(json.dumps(score), flush=True)
    return score


def report(output, variants, scores):
    save_json(output / "scores.json", scores)
    lines = ["| Variant | Runs | On-time skills | Task passed | Verified | Full pass | Errors |", "| --- | ---: | ---: | ---: | ---: | ---: | ---: |"]
    for variant in variants:
        rows = [row for row in scores if row["variant"] == variant["name"]]
        values = [sum(bool(row[key]) for row in rows) for key in ["selection_pass", "task_pass", "verification_pass", "pass", "error"]]
        lines.append("| " + " | ".join(map(str, [variant["name"], len(rows), *values])) + " |")
    (output / "summary.md").write_text("\n".join(lines) + "\n")
    print("\n".join(lines), flush=True)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--variant", action="append", required=True, metavar="NAME=GIT_REF")
    parser.add_argument("--model")
    parser.add_argument("--cases", type=Path, default=Path(__file__).with_suffix(".json"))
    parser.add_argument("--output", type=Path)
    parser.add_argument("--repeats", type=int, default=2)
    parser.add_argument("--jobs", type=int, default=3)
    parser.add_argument("--timeout", type=int, default=180)
    args = parser.parse_args()
    if min(args.repeats, args.jobs, args.timeout) < 1:
        parser.error("repeats, jobs, and timeout must be positive")
    requested = [value.split("=", 1) for value in args.variant]
    if any(len(value) != 2 or not re.fullmatch(r"[a-z0-9_-]{1,20}", value[0]) for value in requested) or len({value[0] for value in requested}) != len(requested):
        parser.error("variants need distinct short names followed by =GIT_REF")
    base = os.environ.get("KYOTOAGENT_E2E_BASE_URL", "").rstrip("/")
    if not base:
        parser.error("set KYOTOAGENT_E2E_BASE_URL")
    with urllib.request.urlopen(base + "/models", timeout=5) as response:
        catalog = json.load(response)
    models = [row["id"] for row in catalog["data"]]
    model = args.model or models[0]
    if model not in models:
        parser.error(f"model {model} is absent from the server catalog")
    repo = Path(git(Path.cwd(), "rev-parse", "--show-toplevel"))
    target = Path(os.environ.get("CARGO_TARGET_DIR", repo / "target")).resolve()
    output = (args.output or Path(f"/tmp/kyoto-eval-{time.time_ns()}")).resolve()
    output.mkdir(parents=True)
    cases = json.loads(args.cases.read_text())
    save_json(output / "cases.json", cases)
    save_json(output / "models.json", catalog)
    variants = [build_variant(repo, output, name, revision, target) for name, revision in requested]
    save_json(output / "manifest.json", {"model": model, "base_url": base, "variants": variants, "repeats": args.repeats, "jobs": args.jobs, "timeout": args.timeout, "runner_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest()})
    servers = []
    scores = []
    try:
        for variant in variants:
            servers.append(start_server(variant, output, base, model))
        tasks = []
        with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
            for repeat in range(args.repeats):
                for index, case in enumerate(cases):
                    offset = (repeat + index) % len(servers)
                    for server in servers[offset:] + servers[:offset]:
                        tasks.append(pool.submit(run_case, server, case, repeat, output, args.timeout))
            for future in concurrent.futures.as_completed(tasks):
                scores.append(future.result())
                save_json(output / "scores.json", scores)
    finally:
        for server in servers:
            stop_server(server)
    report(output, variants, scores)
    print(f"Evidence: {output}", flush=True)
    return 2 if any(row["error"] and row["error"].startswith("infrastructure:") for row in scores) else 0


if __name__ == "__main__":
    raise SystemExit(main())
