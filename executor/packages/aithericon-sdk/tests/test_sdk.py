"""Unit tests for the aithericon SDK in disconnected mode (no gRPC server)."""

import json
import os
import tempfile

import aithericon
from aithericon._client import init, is_connected, _stub, _channel
from aithericon._inputs import load_inputs, token, Token
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


# ── token() / Token ──────────────────────────────────────────────────

def test_token_reads_input_json(monkeypatch):
    with tempfile.TemporaryDirectory() as tmp:
        with open(os.path.join(tmp, "input.json"), "w") as f:
            json.dump({"vendor": "ACME", "amount": 42}, f)
        # An unrelated staged file must not leak into the token.
        with open(os.path.join(tmp, "config.json"), "w") as f:
            json.dump({"lr": 0.001}, f)
        monkeypatch.setenv("AITHERICON_INPUTS_DIR", tmp)

        t = token()

    assert isinstance(t, Token)
    assert t.vendor == "ACME"          # attribute access
    assert t["amount"] == 42           # item access (dict)
    assert t.get("amount") == 42
    assert "lr" not in t               # only input.json, not the file map


def test_token_missing_attr_is_none_but_item_raises():
    t = Token({"present": 1})
    assert t.present == 1
    assert t.absent is None            # forgiving attribute access
    assert t.get("absent") is None
    try:
        t["absent"]
    except KeyError:
        pass
    else:
        raise AssertionError("item access should raise KeyError when absent")


def test_token_nested_wrapping():
    t = Token({"address": {"city": "Berlin"}, "items": [{"sku": "x"}]})
    assert t.address.city == "Berlin"          # nested dict wrapped
    assert t["items"][0].sku == "x"            # dict elements in lists wrapped
    assert isinstance(t.address, Token)


def test_token_empty_when_no_input_json(monkeypatch):
    with tempfile.TemporaryDirectory() as tmp:
        monkeypatch.setenv("AITHERICON_INPUTS_DIR", tmp)
        t = token()
    assert isinstance(t, Token)
    assert t == {}
    assert t.anything is None


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


def test_set_output_writes_file_even_when_ipc_connected(monkeypatch):
    """Regression: the runner template's required-output check reads
    ``{AITHERICON_OUTPUTS_DIR}/{name}.json``. When the SDK was connected
    over IPC it used to skip the file write, which silently failed any
    script that called ``set_output(...)`` instead of bare globals — the
    runner then exited with ``missing required output(s)`` even though
    the IPC call succeeded. Both writes must happen now."""
    calls = []

    class FakeStub:
        def SetOutput(self, request):
            calls.append((request.name, request.value_json))

    with tempfile.TemporaryDirectory() as tmp:
        monkeypatch.setenv("AITHERICON_OUTPUTS_DIR", tmp)
        import aithericon._client as client_mod
        monkeypatch.setattr(client_mod, "_stub", FakeStub())

        set_output("result", {"score": 99})

        # File is the durable contract.
        path = os.path.join(tmp, "result.json")
        assert os.path.exists(path), "set_output must write the file even when IPC is up"
        with open(path) as f:
            assert json.load(f) == {"score": 99}

        # IPC is still called for streaming.
        assert calls == [("result", json.dumps({"score": 99}))]


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
        with open(os.path.join(tmp, "input.json"), "w") as f:
            json.dump({"vendor": "ACME"}, f)

        monkeypatch.setenv("AITHERICON_INPUTS_DIR", tmp)

        with ExecutionContext() as ctx:
            assert ctx.inputs.get("params.json") == {"x": 10}
            assert isinstance(ctx.token, Token)
            assert ctx.token.vendor == "ACME"      # same surface as token()

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
