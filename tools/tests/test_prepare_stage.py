"""No game data or downloads: exercise the adapter against upstream's real AST."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


TOOLS_DIR = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(TOOLS_DIR))
import prepare_stage as adapter


class StageStarted(Exception):
    pass


class StageAdapterTests(unittest.TestCase):
    def setUp(self):
        self.upstream, self.blocks = adapter.load_upstream(TOOLS_DIR / "setup_acnh.py")
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.context = dict(a=argparse.Namespace(game="/synthetic", mod=[], no_build=False),
                            G=str(self.root), logs=str(self.root), cores=2, jobs=2,
                            env=dict(os.environ), py="venv-python", cmake="venv-cmake",
                            ninja="venv-ninja", vkh="vulkan-headers", cc=[], dotnet="dotnet",
                            harness="harness.dll", hlog=str(self.root / "harness.log"))

    def test_each_real_stage_starts_without_running_another_stage(self):
        for stage in adapter.STAGES:
            with self.subTest(stage=stage):
                seen = []

                def say(message):
                    seen.append(message.split()[0])
                    raise StageStarted

                with patch.object(self.upstream, "say", side_effect=say), \
                     patch.object(self.upstream.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "-")):
                    with self.assertRaises(StageStarted):
                        adapter.invoke(self.upstream, self.blocks, stage, self.context)
                self.assertEqual(seen, [stage])

    def test_shader_commands_and_failure_are_confined_to_shaders(self):
        (self.root / "spirv").mkdir()
        (self.root / "spirv" / "summary.txt").write_text("synthetic summary\n")
        self.context["harness"] = str(self.root / "translator.dll")
        Path(self.context["harness"]).touch()
        # A missing summary makes upstream run extraction and translation. The
        # fake command writes a harmless text sentinel instead of any shader.
        (self.root / "spirv" / "summary.txt").unlink()
        commands = []

        def run(command, **kwargs):
            commands.append(command)
            (self.root / "spirv" / "summary.txt").write_text("synthetic summary\n")

        with patch.object(self.upstream, "run", side_effect=run), \
             patch.object(self.upstream.shutil, "which", return_value=None):
            adapter.invoke(self.upstream, self.blocks, "shaders", self.context)
        self.assertEqual(commands, [
            ["venv-python", "tools/extract_shaders.py", str(self.root), "--jobs", "2"],
            ["dotnet", self.context["harness"], "--batch", str(self.root / "shaders_extracted"),
             "--out", str(self.root / "spirv"), "--jobs", "2", "--no-val"],
        ])
        (self.root / "spirv" / "summary.txt").unlink()
        with patch.object(self.upstream, "run", side_effect=SystemExit(7)) as failed:
            with self.assertRaisesRegex(SystemExit, "7"):
                adapter.invoke(self.upstream, self.blocks, "shaders", self.context)
        self.assertEqual(failed.call_count, 1)

    def test_recompile_keeps_module_order_bases_and_jobs(self):
        modules = [("sdk", "0x4010000000"), ("subsdk0", "0x4018000000"), ("main", "0x4000000000")]
        for module, _ in modules:
            (self.root / "gen" / module).mkdir(parents=True)
        self.context["cores"] = 8

        def query(command, **kwargs):
            value = "-" if command[-1] == "hooks" else "3.0.2"
            return subprocess.CompletedProcess(command, 0, value)

        with patch.object(self.upstream.subprocess, "run", side_effect=query), \
             patch.object(self.upstream, "run") as run:
            adapter.invoke(self.upstream, self.blocks, "recompile", self.context)
        self.assertEqual([call.args[0] for call in run.call_args_list], [
            ["venv-python", "-m", "recomp", str(self.root / "exefs" / module),
             str(self.root / "gen" / module), "--name", module, "--base", base, "--jobs", "2"]
            for module, base in modules
        ])

    def test_build_uses_upstream_configure_and_ninja_arguments(self):
        with patch.object(self.upstream, "WIN", False), patch.object(self.upstream, "MAC", False), \
             patch.object(self.upstream, "run") as run:
            adapter.invoke(self.upstream, self.blocks, "build", self.context)
        self.assertEqual([call.args[0] for call in run.call_args_list], [
            ["venv-cmake", "-S", ".", "-B", str(self.root / "build"), "-G", "Ninja",
             "-DCMAKE_MAKE_PROGRAM=venv-ninja", "-DNX_GAME=acnh", "-DCMAKE_BUILD_TYPE=Release",
             f"-DNX_VULKAN_INCLUDE={os.path.join('vulkan-headers', 'include')}", f"-DNX_GEN_DIR={self.root / 'gen'}"],
            ["venv-ninja", "-C", str(self.root / "build"), "-j2", "acnh"],
        ])

    def test_tools_discovery_can_be_reused_without_running_tools_again(self):
        private = self.root / "private"
        third_party = private / "third_party"
        for file in [private / "venv/bin/python",
                     third_party / "imgui/imgui.cpp", third_party / "ryubing/Ryujinx.sln",
                     private / "build/spirv_harness/bin/Release/net10.0/spirv_harness.dll"]:
            file.parent.mkdir(parents=True, exist_ok=True)
            file.touch()
        (third_party / f"Vulkan-Headers-{self.upstream.VK_HEADERS}").mkdir()
        args = argparse.Namespace(stage="tools", out=str(self.root), jobs=2)
        self.addCleanup(os.chdir, Path.cwd())
        with patch.dict(os.environ), patch.object(self.upstream, "P", str(private)), \
             patch.object(self.upstream, "T", str(third_party)), \
             patch.object(self.upstream, "WIN", False), patch.object(self.upstream, "MAC", False), \
             patch.object(self.upstream.shutil, "which", side_effect=lambda tool: f"/tools/{tool}"), \
             patch.object(self.upstream.subprocess, "call", return_value=0), \
             patch.object(self.upstream.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "10.0.401\n")), \
             patch.object(self.upstream, "run") as commands, patch.object(self.upstream, "fetch") as fetch:
            adapter.run_stage(self.upstream, self.blocks, args)
            state = json.loads((self.root / ".prepare-tools.json").read_text())
            self.assertEqual(state["py"], str(private / "venv/bin/python"))
            self.assertEqual(state["env_overrides"]["CC"], "clang")
            commands.assert_not_called()
            fetch.assert_not_called()
            # The following version stage must use the saved venv and run exactly
            # its one upstream command, without re-entering tool discovery.
            args.stage = "version"
            adapter.run_stage(self.upstream, self.blocks, args)
            commands.assert_called_once_with([state["py"], "-I", "tools/acnh_version.py", str(self.root)])
            fetch.assert_not_called()

    def test_build_cannot_succeed_without_an_executable(self):
        state = {key: self.context[key] for key in adapter.TOOLS}
        state.update(env_overrides={}, env_removed=[])
        (self.root / ".prepare-tools.json").write_text(json.dumps(state))
        args = argparse.Namespace(stage="build", out=str(self.root), jobs=2, no_build=False)
        previous = Path.cwd()
        self.addCleanup(os.chdir, previous)
        with patch.object(self.upstream, "run"), \
             patch.object(self.upstream, "MAC", False), patch.object(self.upstream, "WIN", False):
            with self.assertRaisesRegex(RuntimeError, "without the requested native executable"):
                adapter.run_stage(self.upstream, self.blocks, args)

    def test_cli_rejects_non_native_targets_before_doing_work(self):
        native = "windows" if os.name == "nt" else "mac" if sys.platform == "darwin" else "linux"
        for target in ("android", "ios", "windows" if native != "windows" else "linux"):
            with self.subTest(target=target):
                result = subprocess.run([
                    sys.executable, "-B", str(TOOLS_DIR / "prepare_stage.py"), "--stage", "tools",
                    "--target", target, "--game", "/synthetic", "--out", str(self.root), "--jobs", "2",
                ], capture_output=True, text=True)
                self.assertEqual(result.returncode, 2)
                self.assertIn(f"cannot build for {target}", result.stderr)
                self.assertFalse((self.root / "logs").exists())
                self.assertFalse((self.root / ".prepare-tools.json").exists())

    def test_stage_boundary_changes_fail_closed(self):
        path = self.root / "changed_setup.py"
        path.write_text((TOOLS_DIR / "setup_acnh.py").read_text().replace("# ---- shaders -", "# ---- renamed -"))
        with self.assertRaisesRegex(RuntimeError, "stage boundaries changed"):
            adapter.load_upstream(path)


if __name__ == "__main__":
    unittest.main()
