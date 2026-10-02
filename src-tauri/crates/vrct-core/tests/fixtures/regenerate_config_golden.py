"""Regenerate `config_golden.json` from the real Python `Config` (src-python/config.py).

The Rust `settings` module must behave like that class, so this records, with a fixed
environment (stubbed audio devices and compute devices, constant WebSocket token):

* `env`       -- what Rust has to be given: the selectable lists, device lists, the language table
* `order`     -- the keys of config.json in the order Python writes them
* `defaults`  -- the serialisable snapshot straight after `init_config`
* `statics`   -- the non-persisted values (VERSION, selectable lists, ...)
* `props`     -- per property: descriptor kind, whether it is persisted/read only, and `probes`:
                 a battery of values set through the real descriptor with the outcome
                 (the stored value, or an error and its class)
* `loads`     -- whole `load_config` runs over odd config.json files: the snapshot afterwards and the
                 exact text Python wrote back

config.py is executed from a copy of its source whose `__file__` points into a scratch directory,
so the real `src-python/config.json` is never read or written.

Run from anywhere:  python regenerate_config_golden.py
"""

import copy
import json
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
SCRATCH = Path(tempfile.mkdtemp(prefix="vrct_config_golden_"))

# ---- load config.py in isolation -------------------------------------------------------------

module = types.ModuleType("config_isolated")
module.__file__ = str(SCRATCH / "config.py")
sys.modules["config_isolated"] = module
exec(compile((SRC / "config.py").read_text(encoding="utf-8"), str(SRC / "config.py"), "exec"), module.__dict__)

Config = module.Config
ManagedProperty = module.ManagedProperty
ValidatedProperty = module.ValidatedProperty

MIC_DEVICES = {
    "Windows WASAPI": [{"name": "Mic A"}, {"name": "Mic B"}],
    "MME": [{"name": "Mic M"}],
}
SPEAKER_DEVICES = [{"name": "Spk A [Loopback]"}, {"name": "Spk B [Loopback]"}]
COMPUTE_DEVICES = [
    {"device": "cpu", "device_index": 0, "device_name": "cpu", "compute_types": ["auto", "float32", "int8"]},
    {"device": "cuda", "device_index": 0, "device_name": "NVIDIA RTX 4090", "compute_types": ["auto", "float16", "int8"]},
]


class StubDeviceManager:
    def getMicDevices(self):
        return copy.deepcopy(MIC_DEVICES)

    def getSpeakerDevices(self):
        return copy.deepcopy(SPEAKER_DEVICES)

    def getDefaultMicDevice(self):
        return {"host": {"name": "Windows WASAPI"}, "device": {"name": "Mic A"}}

    def getDefaultSpeakerDevice(self):
        return {"device": {"name": "Spk A [Loopback]"}}


module.device_manager = StubDeviceManager()
module.getComputeDeviceList = lambda: copy.deepcopy(COMPUTE_DEVICES)
module.secrets_token_urlsafe = lambda n=32: "TOKEN0"
module.errorLogging = lambda *a, **k: None
module.printLog = lambda *a, **k: None


# The instance made while executing config.py may have a debounce timer pending; it would write
# into the scratch file at a random moment. Cancel it, and turn saveConfig (which only schedules
# that timer) into a no-op for every later instance. load_config still writes through saveConfigToFile.
_first = Config._instance
if _first is not None and getattr(_first, "_timer", None) is not None:
    _first._timer.cancel()
Config.saveConfig = lambda self, *a, **k: None


def fresh() -> "Config":
    Config._instance = None
    return Config()


def jsonable(value):
    """Python values as JSON, keeping int / float / bool apart."""
    if isinstance(value, dict):
        return {str(k): jsonable(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [jsonable(v) for v in value]
    if type(value).__name__ in ("dict_keys", "dict_values"):
        return [jsonable(v) for v in value]
    return value


def write_config(text):
    path = SCRATCH / "config.json"
    if text is None:
        path.unlink(missing_ok=True)
    else:
        path.write_text(text, encoding="utf-8", newline="")


def read_config():
    path = SCRATCH / "config.json"
    return path.read_text(encoding="utf-8") if path.exists() else None


write_config(None)
cfg = fresh()
# Restored before every probe so that one probe never leaks into the next.
def snapshot_copy(value):
    """deepcopy, except that `dict_keys` views (the SELECTABLE_*_LIST attributes) become lists."""
    if type(value).__name__ == "dict_keys":
        return list(value)
    return copy.deepcopy(value)


descriptors = {n: o for n, o in Config.__dict__.items() if isinstance(o, (ManagedProperty, ValidatedProperty))}
PRISTINE = {"_" + n: snapshot_copy(cfg.__dict__["_" + n]) for n in descriptors if "_" + n in cfg.__dict__}

order = list(module.json_serializable_vars.keys())

# ---- probe values ----------------------------------------------------------------------------

GENERIC = [
    None, True, False, 0, 1, -1, 2, 10, 100, 101, 5000, 5001, 0.1, 0.5, 0.85, 0.99, 1.0, 2.5, -0.8,
    "", "x", "1", "2", "3", "4", "en", "ja", "stable", "beta", "show", "auto", "cpu",
    "127.0.0.1", "0.0.0.0", "::", "::1", "192.168.0.1", "not an ip", "#FFFFFF", "VRChat",
    [], ["a"], [1, 2], ["a", "a", "b"], ["a", 1, None, "a"], {}, {"a": 1},
]


def mutations(default):
    """Variations of a dict default: right, wrong-typed, missing and extra keys; recursive one level."""
    out = [copy.deepcopy(default)]
    if not isinstance(default, dict):
        return out
    wrong = [None, "bad", 123, True, 1.5, 0, [], {}, "HMD", "LeftHand", "RightHand", -3, 7.25]
    for key in default:
        for w in wrong:
            m = copy.deepcopy(default)
            m[key] = w
            out.append(m)
        m = copy.deepcopy(default)
        del m[key]
        out.append(m)
        if isinstance(default[key], dict):
            for sub in default[key]:
                for w in wrong:
                    m = copy.deepcopy(default)
                    m[key][sub] = w
                    out.append(m)
                m = copy.deepcopy(default)
                del m[key][sub]
                out.append(m)
    m = copy.deepcopy(default)
    m["extra_key"] = 1
    out.append(m)
    return out


def language_variants(default):
    """For SELECTED_*_LANGUAGES: {tab: {slot: {language, country, enable}}}."""
    out = [copy.deepcopy(default)]
    tab = next(iter(default))
    slot = next(iter(default[tab]))
    for lang, country, enable in [
        ("Japanese", "Japan", True), ("English", "United States", False), ("English", "Japan", True),
        ("Klingon", "Mars", True), ("Japanese", "Japan", "yes"), ("Japanese", "Japan", 1), (None, None, True),
    ]:
        m = copy.deepcopy(default)
        m[tab][slot] = {"language": lang, "country": country, "enable": enable}
        out.append(m)
    m = copy.deepcopy(default)
    m[tab]["9"] = {"language": "Japanese", "country": "Japan", "enable": True}
    out.append(m)
    m = copy.deepcopy(default)
    m["9"] = {"1": {"language": "English", "country": "United States", "enable": False}}
    out.append(m)
    m = copy.deepcopy(default)
    m[tab][slot] = {"language": "Japanese"}
    out.append(m)
    out.append({tab: {slot: "text"}})
    m = copy.deepcopy(default)
    m[tab]["9"] = {"language": "Klingon", "country": "Mars", "enable": True}
    out.append(m)
    m = copy.deepcopy(default)
    m["9"] = {"7": {"language": "Japanese", "country": "Japan", "enable": "no"}}
    out.append(m)
    return out


def probes_for(name, descriptor):
    default = copy.deepcopy(PRISTINE["_" + name])
    values = list(GENERIC)
    values.append(default)
    if isinstance(default, (dict, list)):
        values.extend(mutations(default))
    if name in ("SELECTED_YOUR_LANGUAGES", "SELECTED_TARGET_LANGUAGES"):
        values.extend(language_variants(default))
    # Everything the property could be allowed to take, plus something just outside.
    for attr in dir(cfg):
        if attr.startswith("SELECTABLE_") and attr.endswith("_LIST"):
            lst = getattr(cfg, attr)
            if name.replace("SELECTED_", "SELECTABLE_") + "_LIST" == attr or attr.replace("SELECTABLE_", "SELECTED_")[:-5] == name:
                values.extend(list(lst))
                values.append("definitely-not-in-the-list")
    special = {
        "SELECTED_MIC_HOST": ["Windows WASAPI", "MME", "NoHost", "Nope"],
        "SELECTED_MIC_DEVICE": ["Mic A", "Mic B", "Mic M", "NoDevice", "Nope"],
        "SELECTED_SPEAKER_DEVICE": ["Spk A [Loopback]", "Spk B [Loopback]", "NoDevice", "Nope"],
        "SELECTED_TRANSLATION_COMPUTE_DEVICE": copy.deepcopy(COMPUTE_DEVICES) + [{"device": "cpu"}],
        "SELECTED_TRANSCRIPTION_COMPUTE_DEVICE": copy.deepcopy(COMPUTE_DEVICES) + [{"device": "cpu"}],
        "SELECTED_TRANSLATION_COMPUTE_TYPE": ["auto", "float32", "int8", "float16", "bfloat16"],
        "SELECTED_TRANSCRIPTION_COMPUTE_TYPE": ["auto", "float32", "int8", "float16", "bfloat16"],
        "SELECTED_TRANSLATION_ENGINES": [{"1": "Google", "2": "Nope", "3": None}, {"1": "DeepL"}, {"9": "Google"}],
        "WEBSOCKET_HOST": ["127.0.0.1", "10.0.0.5", "::1", "fe80::1", "0.0.0.0", "::", "0:0:0:0:0:0:0:0", "localhost", "256.1.1.1", "1.2.3"],
        "MIC_WORD_FILTER": [["a", "b", "a"], ["", ""], ["x", None], "str", [["nested"]]],
        "OCR_WINDOW_TITLE": ["VRChat", "", " ", "日本語"],
        "OCR_POLL_INTERVAL_MS": [99, 100, 750, 5000, 5001, True, 100.0],
        "OCR_MIN_CONFIDENCE": [0.09, 0.1, 0.5, 0.99, 1, 0.995, True],
        "OCR_BUBBLE_MIN_TEXT_LENGTH": [0, 1, 2, 50, 51, True],
    }
    values.extend(special.get(name, []))
    return values


def run_probe(name, value):
    for key, saved in PRISTINE.items():
        setattr(cfg, key, copy.deepcopy(saved))
    for key in list(cfg.__dict__):
        if key.startswith("_wrapper_"):
            delattr(cfg, key)
    try:
        setattr(cfg, name, copy.deepcopy(value))
    except Exception as error:  # noqa: BLE001
        return {"error": type(error).__name__}
    return {"ok": jsonable(getattr(cfg, "_" + name))}


props = {}
for name, descriptor in descriptors.items():
    entry = {
        "kind": "validated" if isinstance(descriptor, ValidatedProperty) else "managed",
        "persisted": bool(descriptor.serialize),
    }
    if isinstance(descriptor, ManagedProperty):
        entry["readonly"] = descriptor.readonly
        entry["immediate_save"] = descriptor.immediate_save
        entry["type"] = None if descriptor.type_ is None else (
            [t.__name__ for t in descriptor.type_] if isinstance(descriptor.type_, tuple) else [descriptor.type_.__name__]
        )
    else:
        entry["immediate_save"] = descriptor.immediate_save
    if descriptor.serialize and not (isinstance(descriptor, ManagedProperty) and descriptor.readonly):
        entry["probes"] = [{"input": jsonable(v), "outcome": run_probe(name, v)} for v in probes_for(name, descriptor)]
    props[name] = entry

# ---- defaults, statics, env ------------------------------------------------------------------

for key, saved in PRISTINE.items():
    setattr(cfg, key, copy.deepcopy(saved))

defaults = {k: jsonable(f(cfg)) for k, f in module.json_serializable_vars.items()}
statics = {}
for name, descriptor in descriptors.items():
    if not descriptor.serialize:
        statics[name] = jsonable(getattr(cfg, "_" + name, None))
# Machine specific: Rust derives these from where the app lives.
statics["PATH_LOCAL"] = "<local>"
statics["PATH_CONFIG"] = "<local>/config.json"
statics["PATH_LOGS"] = "<local>/logs"
for extra in ("GROQ_WHISPER_BASE_URL", "OPENAI_WHISPER_BASE_URL"):
    statics[extra] = getattr(Config, extra)
statics["SETUP_DOWNLOAD_URL_stable"] = cfg.SETUP_DOWNLOAD_URL
cfg._SELECTED_RELEASE_CHANNEL = "beta"
statics["SETUP_DOWNLOAD_URL_beta"] = cfg.SETUP_DOWNLOAD_URL
cfg._SELECTED_RELEASE_CHANNEL = "stable"

env = {
    "mic_devices": MIC_DEVICES,
    "speaker_devices": SPEAKER_DEVICES,
    "compute_devices": COMPUTE_DEVICES,
    "transcription_languages": {lang: list(countries.keys()) for lang, countries in module.transcription_lang.items()},
    "ctranslate2_weight_types": list(cfg._SELECTABLE_CTRANSLATE2_WEIGHT_TYPE_LIST),
    "whisper_weight_types": list(cfg._SELECTABLE_WHISPER_WEIGHT_TYPE_LIST),
    "translation_engines": list(cfg._SELECTABLE_TRANSLATION_ENGINE_LIST),
    "transcription_engines": list(cfg._SELECTABLE_TRANSCRIPTION_ENGINE_LIST),
    "ocr_source_languages": list(cfg._SELECTABLE_OCR_SOURCE_LANGUAGE_LIST),
    "token": "TOKEN0",
}

# ---- load_config over odd files --------------------------------------------------------------

LOADS = [
    ("no file", None, None),
    ("empty file", "", None),
    ("empty object", "{}", None),
    ("not json", "{oops", None),
    ("not an object", "[1, 2, 3]", None),
    ("known keys", json.dumps({"OSC_PORT": 9100, "UI_LANGUAGE": "ja", "MIC_THRESHOLD": 120, "FONT_FAMILY": "Meiryo"}), None),
    ("unknown keys dropped", json.dumps({"EVIL": 1, "__class__": 2, "OSC_PORT": 9001, "_OSC_PORT": 5}), None),
    ("read only and unpersisted keys ignored", json.dumps({"VERSION": "9.9.9", "PATH_CONFIG": "x", "ENABLE_TRANSLATION": True, "SELECTABLE_TAB_NO_LIST": ["9"]}), None),
    ("wrong types skipped one by one", json.dumps({"OSC_PORT": "9000", "MIC_THRESHOLD": 77, "ENABLE_CLIPBOARD": "yes", "SEND_MESSAGE_TO_VRC": False}), None),
    ("rejected by allowed", json.dumps({"UI_LANGUAGE": "xx", "WEBSOCKET_HOST": "0.0.0.0", "OSC_PORT": 1234}), None),
    ("validated props", json.dumps({
        "MAIN_WINDOW_GEOMETRY": {"x_pos": 5, "y_pos": "bad", "width": 100, "height": 200},
        "HOTKEYS": {"toggle_vrct_visibility": ["ctrl", "v"], "toggle_translation": "bad", "toggle_transcription_send": None, "toggle_transcription_receive": ["a"]},
        "SELECTED_MIC_DEVICE": "Mic B", "SELECTED_SPEAKER_DEVICE": "Gone",
        "AUTH_KEYS": {"DeepL_API": "key", "Gemini_API": 5, "Unknown": "x"},
        "MIC_WORD_FILTER": ["a", "a", "b"],
    }), None),
    ("websocket token kept", json.dumps({"WEBSOCKET_AUTH_TOKEN": "persisted-token"}), None),
    ("release channel follows the version", json.dumps({"SELECTED_RELEASE_CHANNEL": "stable"}), None),
    ("unicode kept as is", json.dumps({"FONT_FAMILY": "游ゴシック", "OCR_WINDOW_TITLE": "ブイアール"}, ensure_ascii=False), None),
    ("installer language marker", "{}", "ko"),
    ("installer language marker invalid", "{}", "xx"),
    ("installer language wins over the file", json.dumps({"UI_LANGUAGE": "ja"}), "zh-Hans"),
    ("host before device", '{"SELECTED_MIC_HOST": "MME", "SELECTED_MIC_DEVICE": "Mic M"}', None),
    ("device before host", '{"SELECTED_MIC_DEVICE": "Mic M", "SELECTED_MIC_HOST": "MME"}', None),
    ("repeated key", '{"OSC_PORT": 1, "FONT_FAMILY": "A", "OSC_PORT": 2}', None),
    ("nested overlay and format values", json.dumps({
        "OVERLAY_SMALL_LOG_SETTINGS": {"x_pos": 3, "y_pos": 1.5, "z_pos": True, "x_rotation": 0, "y_rotation": 0, "z_rotation": 0,
                                       "display_duration": 9, "fadeout_duration": 1.5, "opacity": 1, "ui_scaling": 2, "tracker": "RightHand"},
        "SEND_MESSAGE_FORMAT_PARTS": {"message": {"prefix": "[", "suffix": "]"}, "separator": " | ",
                                      "translation": {"prefix": "(", "separator": "-", "suffix": ")"}, "translation_first": True},
        "RECEIVED_MESSAGE_FORMAT_PARTS": {"message": {"prefix": 1}},
    }), None),
    ("floats and ints", json.dumps({"MESSAGE_BOX_RATIO": 12.5, "MIC_AVG_LOGPROB": -1, "SELECTED_TAB_NO": "2"}), None),
]

def sort_nested(value):
    if isinstance(value, dict):
        return {k: sort_nested(value[k]) for k in sorted(value)}
    if isinstance(value, list):
        return [sort_nested(v) for v in value]
    return value


loads = []
for title, text, marker in LOADS:
    write_config(text)
    marker_path = SCRATCH / "installer_language.txt"
    marker_path.unlink(missing_ok=True)
    if marker is not None:
        marker_path.write_text(marker + "\n", encoding="utf-8")
    cfg2 = fresh()
    loads.append({
        "title": title,
        "file": text,
        "marker": marker,
        "snapshot": {k: jsonable(f(cfg2)) for k, f in module.json_serializable_vars.items()},
        "runtime": {
            n: jsonable(getattr(cfg2, "_" + n, None))
            for n, d in descriptors.items()
            if not d.serialize and not n.startswith("PATH_")
        },
        "written": read_config(),
        # Python's own json.dump formatting of the same snapshot, nested keys sorted (serde_json sorts
        # them, Python keeps insertion order): what Rust must produce byte for byte.
        "formatted": json.dumps({k: sort_nested(v) for k, v in {k: jsonable(f(cfg2)) for k, f in module.json_serializable_vars.items()}.items()}, indent=4, ensure_ascii=False),
        "marker_left": marker_path.exists(),
    })

# revalidate_selected_models: lists filled, current choice outside them.
write_config(None)
cfg3 = fresh()
cfg3._SELECTABLE_OPENAI_MODEL_LIST = ["m1", "m2"]
cfg3._SELECTED_OPENAI_MODEL = "gone"
cfg3._SELECTABLE_GROQ_MODEL_LIST = ["g1"]
cfg3._SELECTED_GROQ_MODEL = "g1"
cfg3._SELECTABLE_GEMINI_MODEL_LIST = []
cfg3._SELECTED_GEMINI_MODEL = "kept because the list is empty"
cfg3._SELECTABLE_PLAMO_MODEL_LIST = ["p1"]
cfg3._SELECTED_PLAMO_MODEL = None
cfg3.revalidate_selected_models()
revalidate = {
    "lists": {"openai": ["m1", "m2"], "groq": ["g1"], "gemini": [], "plamo": ["p1"]},
    "before": {"openai": "gone", "groq": "g1", "gemini": "kept because the list is empty", "plamo": None},
    "after": {
        "openai": cfg3._SELECTED_OPENAI_MODEL, "groq": cfg3._SELECTED_GROQ_MODEL,
        "gemini": cfg3._SELECTED_GEMINI_MODEL, "plamo": cfg3._SELECTED_PLAMO_MODEL,
    },
}

# Keys already sorted: the Rust side reads them back through serde_json, which sorts.
VERSIONS = ["3.5.1", "3.5.1-beta.1", "3.5.1-rc.2", "3.5.1-RC1", "3.5.1-alpha", "beta", "-beta", "rc", ""]
channels = [{"version": v, "channel": Config._channelForVersion(v)} for v in VERSIONS]

OVERLAY_VALID = {
    "x_pos": 1.5, "y_pos": -2, "z_pos": 0.25, "x_rotation": 10, "y_rotation": 20.5, "z_rotation": 0,
    "display_duration": 8, "fadeout_duration": 3, "opacity": 0.5, "ui_scaling": 2, "tracker": "RightHand",
}
OVERLAY_MIXED = {
    "x_pos": "left", "y_pos": 7, "z_pos": True, "x_rotation": None, "y_rotation": [1], "z_rotation": 4.5,
    "display_duration": 2.5, "fadeout_duration": True, "opacity": "x", "ui_scaling": 3, "tracker": "Nope",
}
SEQUENCES = [
    {"title": "hotkeys fall back to the binding they had", "steps": [
        ["HOTKEYS", {"toggle_vrct_visibility": ["ctrl", "v"], "toggle_translation": ["a"], "toggle_transcription_send": None, "toggle_transcription_receive": None}],
        ["HOTKEYS", {"toggle_vrct_visibility": None, "toggle_translation": "bad", "toggle_transcription_send": ["x"], "toggle_transcription_receive": 5}],
        ["HOTKEYS", {"toggle_vrct_visibility": ["z"], "toggle_translation": {"a": 1}, "toggle_transcription_send": None, "toggle_transcription_receive": None}],
    ]},
    {"title": "API keys fall back to the key they had", "steps": [
        ["AUTH_KEYS", {"DeepL_API": "k1", "Gemini_API": "g1"}],
        ["AUTH_KEYS", {"DeepL_API": None, "Gemini_API": 5, "Groq_API": "gr"}],
        ["AUTH_KEYS", {}],
        ["AUTH_KEYS", {"Unknown": "x"}],
        ["TRANSCRIPTION_AUTH_KEYS", {"Deepgram": "d1", "Groq_Whisper": "w"}],
        ["TRANSCRIPTION_AUTH_KEYS", {"Deepgram": None, "Groq_Whisper": 3, "Nope": "x"}],
    ]},
    {"title": "compute types follow the compute device", "steps": [
        ["SELECTED_TRANSLATION_COMPUTE_DEVICE", COMPUTE_DEVICES[1]],
        ["SELECTED_TRANSLATION_COMPUTE_TYPE", "float16"],
        ["SELECTED_TRANSLATION_COMPUTE_TYPE", "float32"],
        ["SELECTED_TRANSLATION_COMPUTE_DEVICE", COMPUTE_DEVICES[0]],
        ["SELECTED_TRANSLATION_COMPUTE_TYPE", "float16"],
        ["SELECTED_TRANSCRIPTION_COMPUTE_DEVICE", COMPUTE_DEVICES[1]],
        ["SELECTED_TRANSCRIPTION_COMPUTE_TYPE", "float16"],
        ["SELECTED_TRANSCRIPTION_COMPUTE_TYPE", "float32"],
        ["SELECTED_TRANSCRIPTION_COMPUTE_DEVICE", {"device": "cuda"}],
    ]},
    {"title": "translation engines fall back to the engine a tab had", "steps": [
        ["SELECTED_TRANSLATION_ENGINES", {"1": "Google", "2": "DeepL", "3": "CTranslate2"}],
        ["SELECTED_TRANSLATION_ENGINES", {"1": "nope", "2": "Google", "9": "nope", "8": "Google"}],
    ]},
    {"title": "languages fall back to the entry a slot had", "steps": [
        ["SELECTED_YOUR_LANGUAGES", {"1": {"1": {"language": "English", "country": "United States", "enable": False}, "2": {"language": "Japanese", "country": "Japan", "enable": True}}}],
        ["SELECTED_YOUR_LANGUAGES", {"1": {"1": {"language": "Klingon", "country": "x", "enable": True}, "2": {"language": "Japanese", "country": "Japan", "enable": "no"}, "3": {"language": "X"}}, "4": {"1": {"language": "English"}}}],
        ["SELECTED_TARGET_LANGUAGES", {"2": {"3": {"language": "Japanese", "country": "Japan", "enable": True}}}],
        ["SELECTED_TARGET_LANGUAGES", {"2": {"3": {"language": "Japanese", "country": "Nowhere", "enable": True}, "1": {"language": "English", "country": "United States", "enable": False}}}],
    ]},
    {"title": "geometry keeps what it had for entries that are not ints", "steps": [
        ["MAIN_WINDOW_GEOMETRY", {"x_pos": 10, "y_pos": 20, "width": 300, "height": 400}],
        ["MAIN_WINDOW_GEOMETRY", {"x_pos": "a", "y_pos": 2.5, "width": True, "height": None}],
        ["MAIN_WINDOW_GEOMETRY", {"x_pos": 1, "y_pos": 2, "width": 3}],
    ]},
    {"title": "overlay settings are checked entry by entry against what they were", "steps": [
        ["OVERLAY_SMALL_LOG_SETTINGS", OVERLAY_VALID],
        ["OVERLAY_SMALL_LOG_SETTINGS", OVERLAY_MIXED],
        ["OVERLAY_LARGE_LOG_SETTINGS", OVERLAY_VALID],
        ["OVERLAY_LARGE_LOG_SETTINGS", OVERLAY_MIXED],
    ]},
    {"title": "the microphone device is checked against the host chosen before it", "steps": [
        ["SELECTED_MIC_HOST", "MME"],
        ["SELECTED_MIC_DEVICE", "Mic M"],
        ["SELECTED_MIC_HOST", "Windows WASAPI"],
        ["SELECTED_MIC_DEVICE", "Mic M"],
        ["SELECTED_MIC_DEVICE", "Mic B"],
        ["SELECTED_MIC_HOST", "Nope"],
        ["SELECTED_MIC_HOST", "NoHost"],
        ["SELECTED_MIC_DEVICE", "Mic A"],
        ["SELECTED_MIC_DEVICE", "NoDevice"],
        ["SELECTED_SPEAKER_DEVICE", "Spk B [Loopback]"],
        ["SELECTED_SPEAKER_DEVICE", "Mic A"],
    ]},
    {"title": "a selected model must be on its list once the list is known", "steps": [
        ["SELECTED_OPENAI_MODEL", "anything"],
        ["SELECTABLE_OPENAI_MODEL_LIST", ["m1", "m2"]],
        ["SELECTED_OPENAI_MODEL", "m2"],
        ["SELECTED_OPENAI_MODEL", "zzz"],
        ["SELECTED_OPENAI_MODEL", None],
        ["SELECTED_OPENAI_MODEL", 1],
        ["SELECTABLE_OPENAI_MODEL_LIST", []],
        ["SELECTED_OPENAI_MODEL", "zzz"],
        ["SELECTABLE_OPENAI_MODEL_LIST", "not a list"],
        ["SELECTABLE_OPENAI_MODEL_LIST", {"a": 1}],
    ]},
    {"title": "run-time state takes its type but is not persisted", "steps": [
        ["ENABLE_TRANSLATION", True],
        ["ENABLE_TRANSLATION", 1],
        ["ENABLE_TRANSLATION", None],
        ["SELECTABLE_TRANSLATION_ENGINE_STATUS", {"Google": True}],
        ["SELECTABLE_TRANSLATION_ENGINE_STATUS", [1]],
        ["DEEPGRAM_MODEL_LANGUAGES", {"nova-3": ["en", "ja"]}],
    ]},
]


def run_sequence(steps):
    for key, saved in PRISTINE.items():
        setattr(cfg, key, copy.deepcopy(saved))
    for key in list(cfg.__dict__):
        if key.startswith("_wrapper_"):
            delattr(cfg, key)
    results = []
    for name, value in steps:
        try:
            setattr(cfg, name, copy.deepcopy(value))
            results.append({"ok": jsonable(getattr(cfg, "_" + name))})
        except Exception as error:  # noqa: BLE001
            results.append({"error": type(error).__name__})
    return results


sequences = [{"title": q["title"], "steps": q["steps"], "outcomes": run_sequence(q["steps"])} for q in SEQUENCES]

FORMAT_SAMPLES = [
    {},
    {"a": []},
    {"a": {"e": {}, "k": [1, 2.0]}},
    {"字": "日本\n\"x\"\t\u0001"},
    {"big": 12345678901, "f": 1.5, "n": None, "neg": -0.8, "nested": [[1], {"a": [None]}], "t": True},
]
formats = [{"input": sample, "text": json.dumps(sample, indent=4, ensure_ascii=False)} for sample in FORMAT_SAMPLES]

golden = {
    "env": env, "order": order, "defaults": defaults, "statics": statics,
    "props": props, "loads": loads, "revalidate": revalidate, "formats": formats,
    "channels": channels, "sequences": sequences,
}
out = HERE / "config_golden.json"
out.write_text(json.dumps(golden, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
probe_count = sum(len(p.get("probes", [])) for p in props.values())
print(f"{len(props)} properties, {probe_count} probes, {len(loads)} load cases -> {out.name} ({out.stat().st_size // 1024} KiB)")
shutil.rmtree(SCRATCH, ignore_errors=True)
