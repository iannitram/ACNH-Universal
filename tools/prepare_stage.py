#!/usr/bin/env python3
"""Run one unchanged setup_acnh.py stage, without editing upstream's entry point.

Upstream's stages contain several commands and share tool discovery. Extract their
statements from main, keep discovery in a local completion file, and execute only
the requested body. Fail closed if an upstream sync changes the stage boundaries.
This adapter retires with setup_acnh.py in roadmap 1.31.
"""
import argparse
import ast
import copy
import importlib.util
import json
import os
from pathlib import Path
import re
import sys


STAGES = ("tools", "game", "version", "recompile", "shaders", "fonts", "build")
TOOLS = ("py", "cmake", "ninja", "vkh", "cc", "dotnet", "harness", "hlog")


def load_upstream(path):
    source = path.read_text(encoding="utf-8")
    tree = ast.parse(source, filename=str(path))
    main = next(node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == "main")
    boundaries = [(match.group(1), source.count("\n", 0, match.start()) + 1)
                  for match in re.finditer(r"^    # ---- (\w+) -", source, re.MULTILINE)]
    if tuple(name for name, _ in boundaries) != STAGES:
        raise RuntimeError("upstream setup stage boundaries changed; update prepare_stage.py")
    blocks = {}
    for index, (name, start) in enumerate(boundaries):
        end = boundaries[index + 1][1] if index + 1 < len(boundaries) else main.end_lineno + 1
        blocks[name] = [copy.deepcopy(node) for node in main.body if start < node.lineno < end]
        if not blocks[name]:
            raise RuntimeError(f"upstream {name} stage is empty")
    spec = importlib.util.spec_from_file_location("acnh_upstream_setup", path)
    upstream = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(upstream)  # __main__ is deliberately not executed
    return upstream, blocks


def invoke(upstream, blocks, stage, context):
    # Assignments in this function's scope keep ensure_harness's `nonlocal harness`
    # intact, including its alternate output path fallback.
    assignments = ast.parse("\n".join(f"{key} = context[{key!r}]" for key in context)).body
    helpers = []
    if stage != "tools":
        helpers = [copy.deepcopy(node) for node in blocks["tools"] if isinstance(node, ast.FunctionDef)]
    dependencies = []
    if stage == "recompile":
        # Upstream reads hooks/version through acnh_version.py --get. Keep exactly
        # those reads, without repeating the version stage's write.
        dependencies = [copy.deepcopy(node) for node in blocks["version"] if isinstance(node, ast.Assign)]
    function = ast.parse("def run_stage(context):\n    pass\n").body[0]
    function.body = assignments + helpers + dependencies + copy.deepcopy(blocks[stage]) + ast.parse("return locals()").body
    module = ast.fix_missing_locations(ast.Module(body=[function], type_ignores=[]))
    namespace = vars(upstream).copy()
    exec(compile(module, upstream.__file__, "exec"), namespace)
    return namespace["run_stage"](context)


def run_stage(upstream, blocks, args):
    root = Path(args.out).resolve()
    logs = root / "logs"
    logs.mkdir(parents=True, exist_ok=True)
    os.chdir(upstream.ROOT)
    # FORCE regenerates outputs; it is never a stage selector.
    os.environ.pop("FORCE", None)
    cores = os.cpu_count() or 1
    jobs = args.jobs or int(os.environ.get("JOBS") or max(1, min(cores, (upstream.free_mb() - 1000) // 700)))
    context = dict(a=args, G=str(root), logs=str(logs), cores=cores, jobs=jobs)
    state_path = root / ".prepare-tools.json"
    if args.stage != "tools":
        state = json.loads(state_path.read_text(encoding="utf-8"))
        context.update({key: state[key] for key in TOOLS})
        context["env"] = dict(os.environ)
        for key in state["env_removed"]:
            context["env"].pop(key, None)
        context["env"].update(state["env_overrides"])
        # Upstream's recompiler inherits CC/CXX; Windows child processes also need
        # the venv and LLVM paths established by the tools stage.
        for key in ("CC", "CXX"):
            if key in context["env"]:
                os.environ[key] = context["env"][key]
    inherited = dict(os.environ)
    result = invoke(upstream, blocks, args.stage, context)
    if args.stage == "tools":
        state = {key: result[key] for key in TOOLS}
        state["env_overrides"] = {key: value for key, value in result["env"].items() if inherited.get(key) != value}
        state["env_removed"] = [key for key in inherited if key not in result["env"]]
        temporary = state_path.with_suffix(".tmp")
        temporary.write_text(json.dumps(state), encoding="utf-8")
        temporary.replace(state_path)
    if args.stage == "build":
        executable = root / "build" / ("acnh.exe" if upstream.WIN else "acnh")
        if not executable.is_file():
            raise RuntimeError("upstream build succeeded without the requested native executable")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--stage", choices=STAGES, required=True)
    parser.add_argument("--target", choices=("linux", "windows", "mac", "ios", "android"), required=True)
    parser.add_argument("--game", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--jobs", type=int, default=0)
    args = parser.parse_args()
    if args.jobs < 0:
        parser.error("--jobs must be nonnegative")
    native = "windows" if os.name == "nt" else "mac" if sys.platform == "darwin" else "linux"
    if args.target != native:
        parser.error(f"upstream setup cannot build for {args.target} on {native}")
    # The copied game body expects upstream's CLI namespace.
    args.mod, args.no_build = [], False
    upstream, blocks = load_upstream(Path(__file__).with_name("setup_acnh.py"))
    run_stage(upstream, blocks, args)


if __name__ == "__main__":
    main()
