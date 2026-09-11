import json
import os
from pathlib import Path
import shutil
import shlex
import subprocess
import sys
import tempfile
import time
import unittest

ORC = str(Path(sys.argv.pop(1)).resolve())
REPO = Path(__file__).resolve().parents[1]


def shell(source):
    return ["/bin/sh", "-c", source]


def observation(status):
    return shell("printf '%s\\n' '" + json.dumps({"status": status}) + "'")


def resource(kind, name, spec):
    return {"apiVersion": "orc.dev/v1alpha1", "kind": kind,
            "metadata": {"name": name}, "spec": spec}


class Orchestration(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="orc-regression-")
        self.root = Path(self.temp.name)
        self.scope = self.root / "scope"
        self.scope.mkdir()
        self.providers = self.root / "providers"
        self.providers.mkdir()
        self.env = {k: v for k, v in os.environ.items() if not k.startswith("ORC_")}
        for key in ["XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_DATA_HOME",
                    "XDG_DATA_DIRS", "XDG_CACHE_HOME"]:
            self.env[key] = str(self.root / key)
        self.env.update(ORC_PROVIDERS_DIRECTORY=str(self.providers),
                        ORC_WORKFLOWS_REPOSITORY=str(self.root / "catalog"),
                        ORC_WORKFLOWS_AUTO_COMMIT="false", ORC_DAEMON_AUTOSTART="false",
                        ORC_DAEMON_SCAN_INTERVAL_MS="100", ORC_DAEMON_IDLE_SHUTDOWN_SECONDS="1",
                        ZMX_DIR=str(self.root / "zmx"), ZMX_SESSION_PREFIX="")
        for key in ["ZMX_SESSION", "ZMX_PID", "WEZTERM_PANE"]:
            self.env.pop(key, None)
        self.add_provider("local", ["execution.run", "execution.ensure", "execution.observe",
                                    "execution.cancel", "execution.logs"])
        self.children = []

    def tearDown(self):
        for child in self.children:
            if child.poll() is None:
                child.kill()
            child.communicate(timeout=10)
        stopped = subprocess.run([ORC, "stop"], env=self.env, capture_output=True, text=True, timeout=15)
        self.assertEqual(stopped.returncode, 0, stopped.stderr)
        if shutil.which("zmx"):
            listed = subprocess.run(["zmx", "list", "--short"], env=self.env,
                                    capture_output=True, text=True, timeout=10)
            for name in listed.stdout.splitlines():
                subprocess.run(["zmx", "kill", name], env=self.env,
                               capture_output=True, timeout=10)
        self.temp.cleanup()

    def add_provider(self, name, actions):
        manifest = {"version": "orc.provider/v1", "name": name,
                    "kind": {"local": "execution", "harness": "harness", "zmx": "persistence"}[name],
                    "command": str(REPO / "extras" / name / "provider.sh"),
                    "actions": {action: action for action in actions}}
        (self.providers / (name + ".json")).write_text(json.dumps(manifest))

    def command(self, *args):
        return [ORC, *map(str, args), "--scope", str(self.scope)]

    def cmd(self, *args, check=True):
        result = subprocess.run(self.command(*args), env=self.env, capture_output=True,
                                text=True, timeout=20)
        if check:
            self.assertEqual(result.returncode, 0, result.stderr)
        return result

    def start(self, *args):
        child = subprocess.Popen(self.command(*args), env=self.env, stdout=subprocess.PIPE,
                                 stderr=subprocess.PIPE, text=True)
        self.children.append(child)
        return child

    def apply(self, *resources, extra=(), check=True):
        path = self.root / "resources.json"
        path.write_text("\n---\n".join(map(json.dumps, resources)))
        return self.cmd("apply", "-f", path, *extra, check=check)

    def wait_for(self, predicate, timeout=8):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if predicate():
                return
            time.sleep(.05)
        self.fail("condition did not become true before deadline")

    def get(self, kind, name):
        return json.loads(self.cmd("get", kind, name, "-o", "json").stdout)[0]

    def connect(self):
        self.cmd("connect", "--role", "orchestrator", "--id", "root",
                 "--native-id", "native-root", "--harness", "audit", "--quiet")

    def workflow(self, steps):
        self.connect()
        path = self.root / "workflow.json"
        path.write_text(json.dumps({"version": "orc.workflow/v1", "name": "audit",
                                   "goal": "regression", "expected_output": "verified",
                                   "entry_point": steps[0]["name"], "approval": {"mode": "autonomous"},
                                   "steps": steps}))
        return json.loads(self.cmd("workflow", "start", path, "--json").stdout)["id"]

    def run_state(self, run):
        return json.loads(self.cmd("run", "show", run, "--json").stdout)

    def mcp(self, name, arguments):
        messages = [{"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}},
                    {"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                     "params": {"name": name, "arguments": arguments}}]
        env = dict(self.env, ORC_SCOPE=str(self.scope), ORC_SESSION_ID="root")
        result = subprocess.run([ORC, "mcp"], env=env, input="\n".join(map(json.dumps, messages)) + "\n",
                                capture_output=True, text=True, timeout=20)
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout.splitlines()[-1])

    def test_unknown_provider_rejected_without_writes(self):
        item = resource("Execution", "bad", {"provider": "missing", "command": observation("Succeeded")})
        for extra in [("--dry-run",), ()]:
            result = self.apply(item, extra=extra, check=False)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("no available provider", result.stderr)
        self.assertEqual(json.loads(self.cmd("get", "execution", "-o", "json").stdout), [])

    def test_failed_generation_does_not_replay(self):
        self.apply(resource("Execution", "failed", {"provider": "local", "command": shell(
            "echo attempt >> attempts; printf '%s\\n' '{\"status\":\"Failed\"}'")}))
        self.cmd("reconcile")
        self.cmd("reconcile")
        self.assertEqual((self.scope / "attempts").read_text().splitlines(), ["attempt"])

    def test_cancel_blocked_dependency(self):
        self.apply(resource("Execution", "fail", {"provider": "local", "command": observation("Failed")}),
                   resource("Execution", "blocked", {"provider": "local", "dependsOn": ["fail"],
                       "command": observation("Succeeded"), "actions": {"execution.cancel": {
                           "command": observation("Cancelled")}}}))
        self.cmd("delete", "execution", "blocked")
        self.assertEqual([r["metadata"]["name"] for r in json.loads(
            self.cmd("get", "execution", "-o", "json").stdout)], ["fail"])

    def test_inflight_cancel_interrupts_side_effect(self):
        self.apply(resource("Execution", "slow", {"provider": "local", "command": shell(
            "echo started > started; sleep 3; echo late > side-effect; printf '%s\\n' '{\"status\":\"Succeeded\"}'"),
            "actions": {"execution.cancel": {"command": observation("Cancelled")}}}), extra=("--no-reconcile",))
        child = self.start("reconcile")
        self.wait_for(lambda: (self.scope / "started").exists())
        started = time.monotonic()
        self.cmd("delete", "execution", "slow")
        child.communicate(timeout=8)
        self.assertLess(time.monotonic() - started, 2)
        self.assertFalse((self.scope / "side-effect").exists())
        self.assertEqual(json.loads(self.cmd("get", "execution", "-o", "json").stdout), [])

    def test_concurrent_reconcilers_do_not_duplicate_work(self):
        self.apply(resource("Execution", "slow", {"provider": "local", "command": shell(
            "echo attempt >> attempts; sleep 1; printf '%s\\n' '{\"status\":\"Succeeded\"}'")}), extra=("--no-reconcile",))
        first = self.start("reconcile")
        self.wait_for(lambda: (self.scope / "attempts").exists())
        self.cmd("reconcile")
        first.communicate(timeout=8)
        self.assertEqual((self.scope / "attempts").read_text().splitlines(), ["attempt"])

    def test_daemon_observes_async_completion(self):
        self.env["ORC_DAEMON_AUTOSTART"] = "true"
        self.apply(resource("Execution", "async", {"provider": "local", "command": observation("Running"),
            "actions": {"execution.cancel": {"command": observation("Cancelled")},
                        "execution.observe": {"command": shell(
                            "if test -f ready; then printf '%s\\n' '{\"status\":\"Succeeded\"}'; "
                            "else printf '%s\\n' '{\"status\":\"Running\"}'; fi")}}}))
        (self.scope / "ready").touch()
        self.wait_for(lambda: self.get("execution", "async")["status"]["phase"] == "Succeeded")

    def test_declarative_run_visible_in_cli_and_mcp(self):
        self.connect()
        workflow = resource("Workflow", "build", {"stages": [
            {"name": "first", "provider": "local", "command": observation("Succeeded")},
            {"name": "second", "provider": "local", "dependsOn": ["first"], "command": observation("Succeeded")}]})
        run = resource("Run", "build-1", {"workflowRef": "build"})
        response = self.mcp("orc_resource_apply", {"resources": [workflow, run]})
        self.assertNotIn("error", response)
        self.assertFalse(response.get("result", {}).get("isError"), response)
        listed = json.loads(self.cmd("run", "list", "--json").stdout)
        self.assertEqual(listed[0]["id"], "resource:build-1")
        self.assertEqual(listed[0]["status"], "done")
        self.assertEqual(len(listed[0]["nodes"]), 2)
        self.assertEqual(len(listed[0]["edges"]), 1)
        response = self.mcp("orc_run_list", {})
        self.assertIn("resource:build-1", json.dumps(response))

    def test_human_review_blocks_successors_without_rerunning_work(self):
        run = self.workflow([
            {"name": "work", "type": "script", "judge_policy": "human",
             "command": shell("echo attempt >> attempts")},
            {"name": "next", "type": "script", "depends_on": ["work"],
             "command": shell("touch downstream")}])
        self.cmd("run", "resume", run)
        current = self.run_state(run)
        self.assertEqual(current["status"], "waiting")
        self.assertFalse((self.scope / "downstream").exists())
        rejected = self.mcp("orc_run_approve", {"id": run, "resume": False})
        self.assertIn("requires user approval", json.dumps(rejected))
        self.cmd("run", "approve", run)
        self.cmd("run", "resume", run)
        self.assertEqual(self.run_state(run)["status"], "done")
        self.assertTrue((self.scope / "downstream").exists())
        self.assertEqual((self.scope / "attempts").read_text().splitlines(), ["attempt"])

    def test_combined_review_requires_both_actors(self):
        run = self.workflow([{"name": "work", "type": "script", "judge_policy": "llm+human",
                              "command": shell("echo attempt >> attempts")}])
        self.cmd("run", "resume", run)
        rejected = self.cmd("run", "approve", run, check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("orchestrator approval before user approval", rejected.stderr)
        response = self.mcp("orc_run_approve", {"id": run, "resume": False})
        self.assertFalse(response.get("result", {}).get("isError"), response)
        self.assertEqual(self.run_state(run)["status"], "waiting")
        self.cmd("run", "approve", run)
        self.cmd("run", "resume", run)
        self.assertEqual(self.run_state(run)["status"], "done")
        self.assertEqual((self.scope / "attempts").read_text().splitlines(), ["attempt"])

    def test_declarative_run_cancel_uses_resource_store(self):
        self.apply(resource("Workflow", "build", {"stages": [
            {"name": "one", "provider": "local", "command": observation("Running"),
             "actions": {"execution.cancel": {"command": observation("Cancelled")},
                         "execution.observe": {"command": observation("Running")}}}]}),
                   resource("Run", "build-1", {"workflowRef": "build"}))
        self.cmd("run", "cancel", "resource:build-1")
        self.assertEqual(json.loads(self.cmd("get", "run", "-o", "json").stdout), [])
        self.assertEqual(json.loads(self.cmd("run", "list", "--json").stdout), [])

    def test_reconcile_preview_materializes_without_persisting(self):
        self.apply(resource("Workflow", "build", {"stages": [
            {"name": "one", "provider": "local", "command": observation("Succeeded") }]}),
                   resource("Run", "build-1", {"workflowRef": "build"}), extra=("--no-reconcile",))
        preview = json.loads(self.cmd("reconcile", "--dry-run", "--json").stdout)
        self.assertEqual(len(preview["actions"]), 1)
        self.assertEqual(json.loads(self.cmd("get", "execution", "-o", "json").stdout), [])

    def test_replacement_generation_survives_stale_provider_result(self):
        self.apply(resource("Execution", "slow", {"provider": "local", "command": shell(
            "touch started; sleep 3; touch stale; printf '%s\\n' '{\"status\":\"Succeeded\"}'")}), extra=("--no-reconcile",))
        child = self.start("reconcile")
        self.wait_for(lambda: (self.scope / "started").exists())
        self.apply(resource("Execution", "slow", {"provider": "local", "command": observation("Failed")}),
                   extra=("--no-reconcile",))
        child.communicate(timeout=8)
        self.cmd("reconcile")
        self.assertFalse((self.scope / "stale").exists())
        self.assertEqual(self.get("execution", "slow")["status"]["phase"], "Failed")
        self.assertEqual(self.get("execution", "slow")["metadata"]["generation"], 2)

    @unittest.skipUnless(os.name == "posix", "requires a PTY")
    def test_tui_refreshes_declarative_runs(self):
        import fcntl
        import pty
        import select
        import struct
        import termios
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 36, 140, 0, 0))
        terminal = subprocess.Popen(self.command("tui"), env=self.env,
                                    stdin=slave, stdout=slave, stderr=slave)
        os.close(slave)
        output = bytearray()

        def shown(needle):
            if select.select([master], [], [], .1)[0]:
                try:
                    output.extend(os.read(master, 65536))
                except OSError:
                    pass
            return needle in output

        try:
            self.wait_for(lambda: shown(b"\x1b[?1049h"))
            self.apply(resource("Workflow", "ui-flow", {"stages": []}),
                       resource("Run", "visible-run", {"workflowRef": "ui-flow"}),
                       extra=("--no-reconcile",))
            self.wait_for(lambda: shown(b"visible-run"))
        finally:
            os.write(master, b"q")
            deadline = time.monotonic() + 5
            while terminal.poll() is None and time.monotonic() < deadline:
                shown(b"\x00")
            if terminal.poll() is None:
                terminal.kill()
            terminal.wait(timeout=5)
            os.close(master)
        self.assertEqual(terminal.returncode, 0, output.decode(errors="replace"))

    def managed(self, source, limit=None):
        self.assertIsNotNone(shutil.which("zmx"), "Zmx is required for managed lifecycle regressions")
        self.add_provider("harness", ["session.bind", "session.launch", "session.attach"])
        self.add_provider("zmx", ["session.bind", "session.persist", "session.stop"])
        agent = self.root / "agent"
        agent.write_text("#!/bin/sh\nset -eu\n" + source + "\n")
        agent.chmod(0o755)
        registry = self.root / "agents.json"
        registry.write_text(json.dumps({"agents": [{"name": "audit", "command": str(agent), "launch": {}}]}))
        helper = self.root / "process-tree"
        helper.write_text("#!/bin/sh\nexec " + shlex.quote(sys.executable) + " "
                          + shlex.quote(str(REPO / "extras/zmx/process_tree.py")) + ' "$@"\n')
        helper.chmod(0o755)
        self.env["ORC_ZMX_TREE_HELPER"] = str(helper)
        self.env["ORC_AGENT_REGISTRY"] = str(registry)
        self.env["ORC_DAEMON_AUTOSTART"] = "true"
        step = {"name": "worker", "type": "agent", "runtime": {"harness": "audit", "execution": "local"}}
        if limit is not None:
            step["timeoutSeconds"] = limit
        return self.workflow([step])

    def test_managed_completion_waits_for_worker(self):
        run = self.managed("sleep 1; echo completed > artifact")
        started = time.monotonic()
        self.cmd("run", "resume", run)
        self.assertGreater(time.monotonic() - started, 1)
        self.assertEqual(self.run_state(run)["status"], "done")
        self.assertTrue((self.scope / "artifact").exists())

    def test_zero_runtime_limit_is_disabled(self):
        run = self.managed("sleep 1; touch artifact", limit=0)
        self.cmd("run", "resume", run)
        self.assertEqual(self.run_state(run)["status"], "done")
        self.assertTrue((self.scope / "artifact").exists())

    def test_managed_failure_is_not_attach_success(self):
        run = self.managed("sleep 1; exit 7")
        self.cmd("run", "resume", run, check=False)
        self.assertEqual(self.run_state(run)["status"], "failed")

    def test_managed_cancel_stops_persistent_worker(self):
        run = self.managed("touch started; sleep 3; touch artifact")
        resumed = self.start("run", "resume", run)
        self.wait_for(lambda: (self.scope / "started").exists())
        self.cmd("run", "cancel", run)
        resumed.communicate(timeout=10)
        self.assertEqual(self.run_state(run)["status"], "cancelled")
        time.sleep(3)
        self.assertFalse((self.scope / "artifact").exists())

    def test_managed_runtime_limit_stops_worker(self):
        run = self.managed("touch started; sleep 6; echo late > artifact", limit=3)
        self.cmd("run", "resume", run, check=False)
        self.assertEqual(self.run_state(run)["status"], "failed")
        self.assertTrue((self.scope / "started").exists())
        time.sleep(6)
        self.assertFalse((self.scope / "artifact").exists())


if __name__ == "__main__":
    unittest.main(verbosity=2)
