"""Regenerate `setters_golden.json`: what the `/set/data/*` endpoints that only change a setting answer.

The endpoints are read out of `src-python/mainloop.py` and `src-python/controller.py`; those whose
handler touches neither `model` nor `self` (it only reads and writes `config`) are the ones the Rust
router serves itself. Their real source is executed here against the real `Config`, in the same
isolated, stubbed environment as `regenerate_config_golden.py`, and the golden records for each
(endpoint, set-up, payload): the reply, and every setting that differs afterwards. `errors` carries
the error table (code -> message, category, severity) the replies use.

Run from anywhere:  python regenerate_setters_golden.py
"""

import ast
import copy
import json
import re
import shutil
import sys
import tempfile
import types
import warnings
from pathlib import Path

REPO = Path(__file__).resolve().parents[5]
SRC = REPO / "src-python"
sys.path.insert(0, str(SRC))
warnings.simplefilter("ignore")

HERE = Path(__file__).resolve().parent
SCRATCH = Path(tempfile.mkdtemp(prefix="vrct_setters_golden_"))

# ---- Config in isolation, with the stubs of regenerate_config_golden.py ------------------------

module = types.ModuleType("config_isolated")
module.__file__ = str(SCRATCH / "config.py")
sys.modules["config_isolated"] = module
exec(compile((SRC / "config.py").read_text(encoding="utf-8"), str(SRC / "config.py"), "exec"), module.__dict__)

Config = module.Config
ManagedProperty = module.ManagedProperty
ValidatedProperty = module.ValidatedProperty
ConfigValidationError = module.ConfigValidationError

COMPUTE_DEVICES = [
    {"device": "cpu", "device_index": 0, "device_name": "cpu", "compute_types": ["auto", "float32", "int8"]},
    {"device": "cuda", "device_index": 0, "device_name": "NVIDIA RTX 4090", "compute_types": ["auto", "float16", "int8"]},
]


class StubDeviceManager:
    def getMicDevices(self):
        return {"Windows WASAPI": [{"name": "Mic A"}, {"name": "Mic B"}], "MME": [{"name": "Mic M"}]}

    def getSpeakerDevices(self):
        return [{"name": "Spk A [Loopback]"}, {"name": "Spk B [Loopback]"}]

    def getDefaultMicDevice(self):
        return {"host": {"name": "Windows WASAPI"}, "device": {"name": "Mic A"}}

    def getDefaultSpeakerDevice(self):
        return {"device": {"name": "Spk A [Loopback]"}}


module.device_manager = StubDeviceManager()
module.getComputeDeviceList = lambda: copy.deepcopy(COMPUTE_DEVICES)
module.secrets_token_urlsafe = lambda n=32: "TOKEN0"
module.errorLogging = lambda *a, **k: None
module.printLog = lambda *a, **k: None

_first = Config._instance
if _first is not None and getattr(_first, "_timer", None) is not None:
    _first._timer.cancel()
Config.saveConfig = lambda self, *a, **k: None

Config._instance = None
cfg = Config()


def snapshot_copy(value):
    if type(value).__name__ == "dict_keys":
        return list(value)
    return copy.deepcopy(value)


def jsonable(value):
    if isinstance(value, dict):
        return {str(k): jsonable(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [jsonable(v) for v in value]
    if type(value).__name__ in ("dict_keys", "dict_values"):
        return [jsonable(v) for v in value]
    return value


descriptors = {n: o for n, o in Config.__dict__.items() if isinstance(o, (ManagedProperty, ValidatedProperty))}
PRISTINE = {"_" + n: snapshot_copy(cfg.__dict__["_" + n]) for n in descriptors if "_" + n in cfg.__dict__}


def reset():
    for key, saved in PRISTINE.items():
        setattr(cfg, key, copy.deepcopy(saved))
    for key in list(cfg.__dict__):
        if key.startswith("_wrapper_"):
            delattr(cfg, key)


def state():
    return {n: jsonable(snapshot_copy(cfg.__dict__["_" + n])) for n in descriptors if "_" + n in cfg.__dict__}


def diff(before, after):
    return {n: after[n] for n in after if before.get(n) != after[n] or type(before.get(n)) is not type(after[n])}


# ---- the real handlers --------------------------------------------------------------------------

from errors import ErrorCode, VRCTError  # noqa: E402

main_source = (SRC / "mainloop.py").read_text(encoding="utf-8")
PAIRS = re.findall(r'"(/set/data/[a-z0-9_]+)": \{"status": (True|False), "variable":controller\.(\w+)\}', main_source)

controller_source = (SRC / "controller.py").read_text(encoding="utf-8")
controller_tree = ast.parse(controller_source)
controller_class = next(n for n in controller_tree.body if isinstance(n, ast.ClassDef) and n.name == "Controller")
methods = {n.name: n for n in controller_class.body if isinstance(n, ast.FunctionDef)}
decorator_source = next(
    ast.get_source_segment(controller_source, n)
    for n in controller_tree.body
    if isinstance(n, ast.FunctionDef) and n.name == "_configValidationErrorResponse"
)

namespace = {
    "config": cfg,
    "ConfigValidationError": ConfigValidationError,
    "ErrorCode": ErrorCode,
    "VRCTError": VRCTError,
    "printLog": lambda *a, **k: None,
    "functools": __import__("functools"),
}
exec(decorator_source, namespace)

pure = []  # (endpoint, name, decorator text or None)
for endpoint, status, name in PAIRS:
    method = methods[name]
    body_text = ast.get_source_segment(controller_source, method)
    if re.search(r"\b(model|self|device_manager)\.", body_text):
        continue
    method.decorator_list = [d for d in method.decorator_list if not (isinstance(d, ast.Name) and d.id == "staticmethod")]
    module_ast = ast.Module(body=[method], type_ignores=[])
    ast.fix_missing_locations(module_ast)
    exec(compile(module_ast, "controller_setter", "exec"), namespace)
    decorated = bool(method.decorator_list)
    code = None
    if decorated:
        code = re.search(r"ErrorCode\.(\w+)", ast.unparse(method.decorator_list[0])).group(1)
    pure.append({"endpoint": endpoint, "name": name, "status_open": status == "True", "decorated_with": code})

# ---- probes ------------------------------------------------------------------------------------

PAYLOADS = [
    None, True, False, 0, 1, -1, 2, 3, 5, 10, 11, 20, 50, 100, 101, 1999, 2000, 2001, 5000, 5001,
    2**31, 2**63, -(2**63),
    0.0, 0.5, 1.0, 1.5, 2.9, -0.5, -0.8, 0.85, 99.99, 1e19, 1e-7, 1e15, 1e16, 1.5e16, 1.5e17, 1.5e-5, 0.0001, 0.00001, 123456.0, 123456789.123456789,
    "", " ", "0", "1", "5", "10", "11", "-1", " 7 ", "+5", "1_000", "1.5", "5.0", "abc", "1e3", "0x10",
    "10\n", "1_0", "1__0", "_1", "1_", "1e1_0", "-0", "+0", ".5", "5.", "1e-3", "\t8\t", "ja", "en", "stable", "beta", "auto", "show", "left", "x", "#FFFFFF",
    "m1", "m2", "whisper-large-v3", "float32", "int8", "float16", "tiny", "large-v3", "Arial", "Segoe UI",
    [], ["a"], ["it's"], ["say \"hi\" it's"], ["a\nb\\c"], [1, 2], [["a", 1]], [[1, 2, 3]], [["abc", 1]], [["a", "b"]], ["ab"], ["abc"], [[1, 2]], [[[1], 2]], [["a"]],
    {}, {"a": 1}, {"a": "b"}, {"1": 2},
]

DEFAULT_FORMAT = None  # filled below once the defaults are known
OVERLAY_LIKE = [
    {"x_pos": 1},
]

# Set-ups: settings assigned (through the real descriptors) before the probes run.
SETUPS = {
    "default": [],
    "timeouts_low": [["MIC_RECORD_TIMEOUT", 2], ["MIC_PHRASE_TIMEOUT", 3], ["SPEAKER_RECORD_TIMEOUT", 2], ["SPEAKER_PHRASE_TIMEOUT", 3]],
    "timeouts_zero": [["MIC_RECORD_TIMEOUT", 0], ["MIC_PHRASE_TIMEOUT", 0], ["SPEAKER_RECORD_TIMEOUT", 0], ["SPEAKER_PHRASE_TIMEOUT", 0]],
    "model_lists": [
        ["SELECTABLE_GROQ_WHISPER_MODEL_LIST", ["m1", "m2"]],
        ["SELECTABLE_OPENAI_WHISPER_MODEL_LIST", ["m1", "m2"]],
        ["SELECTABLE_CUSTOM_WHISPER_MODEL_LIST", ["m1", "m2"]],
    ],
    "cuda_selected": [["SELECTED_TRANSCRIPTION_COMPUTE_DEVICE", COMPUTE_DEVICES[1]]],
}


def apply_setup(steps):
    for name, value in steps:
        try:
            setattr(cfg, name, copy.deepcopy(value))
        except Exception:  # noqa: BLE001
            pass


def call(name, payload):
    try:
        response = namespace[name](copy.deepcopy(payload))
        return {"status": response.get("status", 500), "result": jsonable(response.get("result"))}
    except Exception as error:  # noqa: BLE001  (mainloop turns any exception into 500 "Internal error")
        return {"status": 500, "result": "Internal error", "raised": type(error).__name__}


format_defaults = {
    "SEND_MESSAGE_FORMAT_PARTS": jsonable(snapshot_copy(cfg.__dict__["_SEND_MESSAGE_FORMAT_PARTS"])),
    "RECEIVED_MESSAGE_FORMAT_PARTS": jsonable(snapshot_copy(cfg.__dict__["_RECEIVED_MESSAGE_FORMAT_PARTS"])),
}


def extra_payloads(name):
    out = []
    if name.endswith("FormatParts"):
        base = format_defaults["SEND_MESSAGE_FORMAT_PARTS" if name.startswith("setSend") else "RECEIVED_MESSAGE_FORMAT_PARTS"]
        out.append(copy.deepcopy(base))
        for key in list(base):
            changed = copy.deepcopy(base)
            changed[key] = "bad"
            out.append(changed)
            gone = copy.deepcopy(base)
            del gone[key]
            out.append(gone)
        out.append({**copy.deepcopy(base), "extra": 1})
        out.append([[k, v] for k, v in base.items()])
    if name == "setHotkeys":
        base = jsonable(snapshot_copy(cfg.__dict__["_HOTKEYS"]))
        out.append(copy.deepcopy(base))
        for key in list(base):
            for w in (None, "bad", ["ctrl", "x"], 5):
                changed = copy.deepcopy(base)
                changed[key] = w
                out.append(changed)
    if name == "setMainWindowGeometry":
        base = jsonable(snapshot_copy(cfg.__dict__["_MAIN_WINDOW_GEOMETRY"]))
        out.append(copy.deepcopy(base))
        for key in list(base):
            for w in (None, "x", 7.5, -3, True):
                changed = copy.deepcopy(base)
                changed[key] = w
                out.append(changed)
        short = copy.deepcopy(base)
        short.pop(next(iter(short)))
        out.append(short)
    return out


def setups_for(name):
    """The state-dependent endpoints are probed from several states, the rest from the defaults."""
    if "Timeout" in name:
        return ["default", "timeouts_low", "timeouts_zero"]
    if name in ("setGroqWhisperModel", "setOpenAIWhisperModel", "setCustomWhisperModel"):
        return ["default", "model_lists"]
    if name == "setSelectedTranscriptionComputeType":
        return ["default", "cuda_selected"]
    return ["default"]


results = []
for entry in pure:
    name = entry["name"]
    payloads = PAYLOADS + extra_payloads(name)
    for setup_name in setups_for(name):
        steps = SETUPS[setup_name]
        for payload in payloads:
            reset()
            apply_setup(steps)
            before = state()
            reply = call(name, payload)
            after = state()
            results.append({
                "endpoint": entry["endpoint"], "setup": setup_name, "payload": payload,
                "reply": reply, "changed": diff(before, after),
            })

# ---- sequences: the state a reply leaves behind is the next one's input ------------------------

SEQUENCES = [
    {"title": "mic timeouts check each other", "steps": [
        ["/set/data/mic_phrase_timeout", 10], ["/set/data/mic_record_timeout", 8], ["/set/data/mic_record_timeout", 11],
        ["/set/data/mic_phrase_timeout", 7], ["/set/data/mic_phrase_timeout", 8], ["/set/data/mic_record_timeout", 8],
    ]},
    {"title": "speaker timeouts check each other", "steps": [
        ["/set/data/speaker_phrase_timeout", 10], ["/set/data/speaker_record_timeout", 8], ["/set/data/speaker_record_timeout", 11],
        ["/set/data/speaker_phrase_timeout", 7], ["/set/data/speaker_phrase_timeout", 8], ["/set/data/speaker_phrase_timeout", -1],
    ]},
    {"title": "a rejected value leaves the old one", "steps": [
        ["/set/data/mic_threshold", 300], ["/set/data/mic_threshold", 5000], ["/set/data/mic_threshold", "abc"], ["/set/data/mic_threshold", "450"],
        ["/set/data/ui_language", "ja"], ["/set/data/ui_language", "klingon"], ["/set/data/ui_scaling", 120], ["/set/data/ui_scaling", "wide"],
    ]},
    {"title": "log probabilities are floats whatever came in", "steps": [
        ["/set/data/mic_avg_logprob", 1], ["/set/data/mic_avg_logprob", "2.5"], ["/set/data/mic_avg_logprob", True],
        ["/set/data/speaker_no_speech_prob", 0], ["/set/data/speaker_no_speech_prob", None],
    ]},
    {"title": "whisper models must be on their list", "steps": [
        ["/set/data/selected_groq_whisper_model", "m1"], ["/set/data/selected_groq_whisper_model", 1],
    ]},
]
sequence_results = []
for sequence in SEQUENCES:
    reset()
    apply_setup(SETUPS["model_lists"] if "whisper" in sequence["title"] else [])
    steps_out = []
    for endpoint, payload in sequence["steps"]:
        name = next(e["name"] for e in pure if e["endpoint"] == endpoint)
        before = state()
        reply = call(name, payload)
        steps_out.append({"endpoint": endpoint, "payload": payload, "reply": reply, "changed": diff(before, state())})
    sequence_results.append({"title": sequence["title"], "setup": "model_lists" if "whisper" in sequence["title"] else "default", "steps": steps_out})

# ---- the error table the replies use -----------------------------------------------------------

codes = sorted({r["reply"]["result"]["error_code"] for r in results if isinstance(r["reply"]["result"], dict) and "error_code" in r["reply"]["result"]})
errors = {}
for code in codes:
    result = VRCTError.create_error_response(ErrorCode(code), data=None)["result"]
    errors[code] = {"message": result["message"], "category": result["category"], "severity": result["severity"]}

golden = {"endpoints": pure, "setups": SETUPS, "results": results, "sequences": sequence_results, "errors": errors}
out = HERE / "setters_golden.json"
out.write_text(json.dumps(golden, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
print(f"{len(pure)} endpoints, {len(results)} probes, {len(sequence_results)} sequences, {len(errors)} error codes -> {out.name} ({out.stat().st_size // 1024} KiB)")
shutil.rmtree(SCRATCH, ignore_errors=True)
