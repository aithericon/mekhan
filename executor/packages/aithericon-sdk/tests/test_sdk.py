"""Unit tests for the aithericon SDK in disconnected mode (no gRPC server)."""

import json
import os
import tempfile

import aithericon
from aithericon._client import init, is_connected, _stub, _channel
from aithericon._inputs import load_inputs
from aithericon._outputs import set_output
from aithericon._context import ExecutionContext


# ── load_inputs ──────────────────────────────────────────────────────

def test_load_inputs_json_and_text():
    with tempfile.TemporaryDirectory() as tmp:
        # JSON file
        with open(os.path.join(tmp, "config.json"), "w") as f:
            json.dump({"lr": 0.001}, f)
        # Plain text file
        with open(os.path.join(tmp, "notes.txt"), "w") as f:
            f.write("hello world")

        result = load_inputs(inputs_dir=tmp)

    assert result["config.json"] == {"lr": 0.001}
    assert result["notes.txt"] == "hello world"


def test_load_inputs_empty_dir():
    with tempfile.TemporaryDirectory() as tmp:
        result = load_inputs(inputs_dir=tmp)
    assert result == {}


def test_load_inputs_missing_dir():
    result = load_inputs(inputs_dir="/nonexistent_path_that_does_not_exist")
    assert result == {}


# ── set_output (file fallback) ───────────────────────────────────────

def test_set_output_file_fallback(monkeypatch):
    with tempfile.TemporaryDirectory() as tmp:
        monkeypatch.setenv("AITHERICON_OUTPUTS_DIR", tmp)
        # Ensure no IPC stub is connected
        import aithericon._client as client_mod
        monkeypatch.setattr(client_mod, "_stub", None)

        set_output("result", {"score": 42})

        path = os.path.join(tmp, "result.json")
        assert os.path.exists(path)
        with open(path) as f:
            assert json.load(f) == {"score": 42}


def test_set_output_noop_without_env(monkeypatch):
    monkeypatch.delenv("AITHERICON_OUTPUTS_DIR", raising=False)
    import aithericon._client as client_mod
    monkeypatch.setattr(client_mod, "_stub", None)

    # Should not crash and should not write anything
    set_output("result", 42)


# ── init / is_connected ─────────────────────────────────────────────

def test_init_without_socket(monkeypatch):
    import aithericon._client as client_mod
    monkeypatch.setattr(client_mod, "_channel", None)
    monkeypatch.setattr(client_mod, "_stub", None)
    monkeypatch.delenv("AITHERICON_IPC_SOCKET", raising=False)

    init()
    assert is_connected() is False


# ── ExecutionContext ─────────────────────────────────────────────────

def test_context_manager_disconnected(monkeypatch):
    import aithericon._client as client_mod
    monkeypatch.setattr(client_mod, "_channel", None)
    monkeypatch.setattr(client_mod, "_stub", None)
    monkeypatch.delenv("AITHERICON_IPC_SOCKET", raising=False)

    with tempfile.TemporaryDirectory() as tmp:
        with open(os.path.join(tmp, "params.json"), "w") as f:
            json.dump({"x": 10}, f)

        monkeypatch.setenv("AITHERICON_INPUTS_DIR", tmp)

        with ExecutionContext() as ctx:
            assert ctx.inputs.get("params.json") == {"x": 10}

    # No crash on __exit__


# ── No-op safety for IPC-only functions ──────────────────────────────

def test_noop_functions_dont_crash(monkeypatch):
    import aithericon._client as client_mod
    monkeypatch.setattr(client_mod, "_stub", None)

    # None of these should raise when disconnected
    aithericon.log_artifact("/tmp/fake.txt", name="fake")
    aithericon.update_progress(0.5, message="halfway")
    aithericon.define_phases(["train", "eval"])
    aithericon.update_phase("train", "running")
    aithericon.log_info("info message")
    aithericon.log_warn("warn message")
    aithericon.log_error("error message")
    aithericon.log_debug("debug message")
    aithericon.log_metric("loss", 0.5)
    aithericon.log_metrics([{"name": "acc", "value": 0.9}])
