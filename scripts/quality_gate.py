"""Flowdepth checks. Standard library only; never launches the application GUI."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
SUMMARY = re.compile(r"test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored;")
DOCS = {"README.md", "REVIEW.md", "CONTRIBUTING.md", "SECURITY.md",
        "CODE_OF_CONDUCT.md", "CLAUDE.md", "AGENTS.md"}


def git(*args):
    return subprocess.check_output(["git", "-c", "core.safecrlf=false", "-c", "core.quotePath=false", *args], cwd=ROOT, stderr=subprocess.PIPE)


def documentation(path):
    return path.endswith(".md") and (path.startswith("docs/") or path in DOCS)


def changed_paths(comparison, include_untracked=False):
    paths = git("diff", "--name-only", "--no-renames", "-z", *comparison).split(b"\0")
    if include_untracked:
        paths += git("ls-files", "--others", "--exclude-standard", "-z").split(b"\0")
    return [os.fsdecode(path) for path in paths if path]


def documentation_only(paths):
    return bool(paths) and all(map(documentation, paths))


def fingerprint():
    digest = hashlib.sha256(git("rev-parse", "HEAD") + git("diff", "--binary", "HEAD"))
    for raw in sorted(git("ls-files", "--others", "--exclude-standard", "-z").split(b"\0")):
        if raw:
            p = ROOT / os.fsdecode(raw)
            digest.update(raw)
            digest.update(p.read_bytes() if p.is_file() else b"not-a-file")
    return digest.hexdigest()


def test_verdict(code, output, minimum=0, expected=(), allow_ignored=False):
    if code != 0:
        return "FAIL", "Command returned nonzero"
    if minimum < 0:
        match = re.search(r"Ran (\d+) tests?", output)
        return ("PASS", "Runner tests executed") if match and int(match[1]) > 0 else ("NOT VERIFIED", "No runner tests executed")
    if minimum:
        results = [tuple(map(int, m)) for m in SUMMARY.findall(output)]
        if not results or sum(r[0] for r in results) < minimum:
            return "NOT VERIFIED", "Missing test summary or too few executed tests"
        if any(r[1] for r in results):
            return "FAIL", "Failed tests in summary"
        if any(not re.search(r"test [^\n]*" + re.escape(name) + r" \.\.\. ok", output)
               for name in expected):
            return "NOT VERIFIED", "Required named regression did not execute"
        if any(r[2] for r in results) and not allow_ignored:
            return "NOT VERIFIED", "Ignored tests remain; see log (helpers may be exercised by parents)"
    return "PASS", "Executed successfully"


def run_check(name, command, folder, env, timeout, minimum=0, expected=()):
    start = time.monotonic()
    log = folder / (name + ".log")
    reason = ""
    try:
        with log.open("w", encoding="utf-8") as output:
            options = {"creationflags": subprocess.CREATE_NO_WINDOW} if os.name == "nt" else {"start_new_session": True}
            process = subprocess.Popen(command, cwd=ROOT, env=env, stdout=output,
                                       stderr=subprocess.STDOUT, **options)
            try:
                code = process.wait(timeout=timeout)
            except (subprocess.TimeoutExpired, KeyboardInterrupt):
                # Only terminate this check's owned process tree, never an application session.
                if os.name == "nt":
                    subprocess.run(["taskkill", "/PID", str(process.pid), "/T", "/F"],
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                   creationflags=subprocess.CREATE_NO_WINDOW)
                else:
                    os.killpg(process.pid, signal.SIGKILL)
                process.wait()
                code, reason = -1, "Check timed out or was interrupted"
        output = log.read_text(encoding="utf-8", errors="replace")
        status, detail = test_verdict(code, output, minimum, expected, allow_ignored=name == "workspace")
        ignored = sum(int(row[2]) for row in SUMMARY.findall(output)) if minimum > 0 else 0
        reason = reason or detail
    except OSError as error:
        code, status, reason, ignored = None, "NOT VERIFIED", str(error), 0
    result = dict(name=name, status=status, reason=reason, command=command,
                  exit_code=code, ignored=ignored, seconds=round(time.monotonic()-start, 2), log=str(log))
    print(f"{status}: {name} ({result['seconds']}s) — {reason}", flush=True)
    return result


def plan(mode, cargo, online=False, docs_only=False):
    def test(selection, filter_name=None, features=None):
        cmd = [cargo, "test", *selection, "--release", "--locked"]
        if not online:
            cmd += ["--offline"]
        if features:
            cmd += ["--features", features]
        if filter_name:
            cmd += [filter_name]
        return cmd
    if docs_only:
        return []
    root = ["-p", "flowsurface", "--bin", "flowsurface"]
    exchange = ["-p", "flowsurface-exchange", "--lib"]
    checks = [
        ("format", [cargo, "fmt", "--all", "--", "--check"], 0, ()),
        ("compile", [cargo, "check", "--workspace", "--all-targets", "--locked", "--profile", "release"]
         + ([] if online else ["--offline"]), 0, ()),
        ("fd001-ws03", test(root, "ws03"), 8,
         ("ws03_id_paging_keeps_entire_saturated_millisecond_and_both_boundaries",
          "ws03_partial_id_gap_conflict_empty_and_error_never_yield_proof",
          "ws03_atomic_overlap_rebuild_preserves_sides_boundaries_and_unaffected_bucket",
          "ws03_current_book_proof_does_not_heal_depth_history_raw_or_auxiliary")),
        ("fd001-integrity", test(root, "ws_integrity"), 8, ()),
        ("fd001-bounded", test(exchange, "adapter::ws::tests", "reqwest/cookies"), 10,
         ("overflow_stops_reader_before_wait_and_marker_precedes_new_generation",
          "ws03_proof_overflow_is_reported_as_loss_not_a_successful_boundary")),
        ("fd001-unbounded", test(exchange, "adapter::ws::tests", "reqwest/cookies,unbounded-channel"), 6, ()),
        ("fd001-book-gap", test(exchange, "applies_snapshot_boundary_then_detects_pu_gap", "reqwest/cookies"), 1, ()),
    ]
    # Reuse real subprocess/profile isolation tests. No layouts copied, GUI or keychain access.
    for name in ("profile_arguments_and_relative_roots_are_explicit",
                 "independent_profiles_can_run_together_and_reopen_after_drop",
                 "second_process_cannot_touch_profile_and_process_exit_releases_lock",
                 "initialized_profiles_isolate_state_cache_and_logs"):
        checks.append(("smoke-" + name, test(["-p", "flowsurface", "-p", "flowsurface-data"],
                       "profile::tests::" + name) + ["--", "--exact"], 1, (name,)))
    if mode == "full":
        checks.append(("workspace", test(["--workspace", "--no-fail-fast"]), 1, ()))
        for folder, namespace in (("data/indicators/sources", "core"), ("indicators/sources", "catalog")):
            checks.append(("indicators-" + namespace,
                           [cargo, "run", "--release", "--locked"] + ([] if online else ["--offline"])
                           + ["-p", "flowsurface-indicator-engine", "--bin", "flowsurface-indicator", "--",
                              "test", folder, "--namespace", namespace], 0, ()))
    return checks


def verdict(results):
    statuses = {r["status"] for r in results}
    return "FAIL" if "FAIL" in statuses else "NOT VERIFIED" if "NOT VERIFIED" in statuses else "PASS"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=["fast", "full"], default="fast")
    parser.add_argument("--online", action="store_true", help="Allow Cargo dependency downloads (CI)")
    parser.add_argument("--allow-docs-only", action="store_true", help="Skip Rust only if all changed paths are allowlisted Markdown")
    parser.add_argument("--base")
    parser.add_argument("--head")
    parser.add_argument("--require", choices=["automated", "all"], default="automated")
    parser.add_argument("--timeout", type=int, default=1800, help="Seconds per check")
    args = parser.parse_args(argv)
    if bool(args.base) != bool(args.head) or args.timeout < 1:
        parser.error("Supply both --base and --head; timeout must be positive")
    if args.base and not all(re.fullmatch(r"[0-9a-fA-F]{7,64}", sha) for sha in (args.base, args.head)):
        parser.error("--base and --head must be commit SHAs")
    home = ROOT / ".local" / "quality-gate"
    try:
        if home.resolve() != home:
            raise OSError("Quality Gate directory resolves through a symlink or junction")
        home.mkdir(parents=True, exist_ok=True)
    except OSError as error:
        print(f"NOT VERIFIED: {error}")
        return 2
    lock = home / "run.lock"
    try:
        with lock.open("x") as handle:
            handle.write(str(os.getpid()))
    except FileExistsError:
        print("NOT VERIFIED: another gate owns .local/quality-gate/run.lock; inspect its PID before removing a stale lock")
        return 2
    try:
        folder = home / (datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ") + f"-{os.getpid()}")
        folder.mkdir()
        env = os.environ.copy()
        env["PATH"] = str(Path.home()/".cargo"/"bin") + os.pathsep + env.get("PATH", "")
        env["CARGO_TERM_COLOR"] = "never"
        env["PYTHONDONTWRITEBYTECODE"] = "1"
        # Defensive isolation for test code. Never reuse the prepared interactive profile.
        profile = folder / "profile"
        profile.mkdir()
        env["FLOWSURFACE_DATA_PATH"] = str(profile)
        env["FLOWSURFACE_LOG_DIR"] = str(profile / "logs")
        start = fingerprint()
        comparison = [args.base + "..." + args.head] if args.base else ["HEAD"]
        docs_only = args.allow_docs_only and documentation_only(changed_paths(comparison, not args.base))
        results = [run_check("diff", ["git", "diff", "--check", *comparison], folder, env, args.timeout),
                   run_check("runner-tests", [sys.executable, "-m", "unittest", "discover", "-s", "scripts", "-p", "test_quality_gate.py"], folder, env, args.timeout, -1)]
        cargo = shutil.which("cargo", path=env["PATH"]) or "cargo"
        for name, command, minimum, expected in plan(args.mode, cargo, args.online, docs_only):
            results.append(run_check(name, command, folder, env, args.timeout, minimum, expected))
        if fingerprint() != start:
            results.append(dict(name="source-stability", status="NOT VERIFIED", reason="Tracked/untracked sources changed during checks; rerun on stable input"))
        automated = verdict(results)
        manual = [] if docs_only else [dict(name="FD-001 UI/live", status="NOT VERIFIED",
            reason="No GUI or exchange session launched. In a NEW isolated profile: BTCUSDT spot/perp, reconnect/overflow replay, switch symbol/tab, check incomplete warning and supported recovery; unsupported/raw/liquidations/depth history stay incomplete. Close normally and reopen. Never edit shared keychain. Record build, UTC range, units and observed result; do not induce overflow in working session.")]
        for row in results:
            if row["name"] == "workspace" and row["ignored"]:
                manual.append(dict(name="Workspace ignored tests", status="NOT VERIFIED",
                                   reason=f"{row['ignored']} ignored in Cargo output. See workspace.log for test names and reasons."))
        report = dict(mode=args.mode, docs_only=docs_only, commit=git("rev-parse", "HEAD").decode().strip(),
                      source_sha256=start, utc=datetime.now(timezone.utc).isoformat(), checks=results,
                      automated=automated, manual=manual, overall=verdict(results+manual))
        (folder/"report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
        if env.get("GITHUB_STEP_SUMMARY"):
            with open(env["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as summary:
                summary.write(f"## Quality Gate ({args.mode})\nAutomated: **{automated}**; overall: **{report['overall']}**\n\n")
                summary.write("| Check | Result |\n|---|---|\n")
                for row in results + manual:
                    summary.write(f"| {row['name']} | {row['status']} |\n")
                for row in manual:
                    summary.write("\n" + row["reason"] + "\n")
        print(f"Automated: {automated}; overall: {report['overall']}; report: {folder/'report.json'}", flush=True)
        status = report["overall"] if args.require == "all" else automated
        return {"PASS": 0, "FAIL": 1, "NOT VERIFIED": 2}[status]
    except Exception as error:
        report = dict(overall="NOT VERIFIED", automated="NOT VERIFIED", error=str(error))
        if "folder" in locals():
            (folder/"report.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
            print(f"NOT VERIFIED: gate infrastructure error; report: {folder/'report.json'}")
        else:
            print(f"NOT VERIFIED: {error}")
        return 2
    finally:
        lock.unlink()


if __name__ == "__main__":
    sys.exit(main())
