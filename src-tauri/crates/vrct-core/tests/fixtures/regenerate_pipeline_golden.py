"""Regenerate `pipeline_golden.json`: how a transcript, a chat message or an OCR line travels through VRCT.

The REAL `Controller._processMessage`, `micMessage`, `speakerMessage`, `ocrMessage`, `chatMessage`,
`_disableTranscriptionAfterPipelineError`, `messageFormatter` and `_is_overlay_available` are taken out of
`src-python/controller.py`, and the REAL `Model.checkKeywords`, `detectRepeat*Message`, `addKeywords`,
`addTranslationHistory` and `detectVRAMError` out of `src-python/model.py` (by AST, so their code is exactly
VRCT's and the heavy imports are not needed). They run against a scripted `config`, a `model` whose outputs
(translator, transliterator, OSC, overlay, clipboard, WebSocket, logger) only record what they are asked,
and a `run` that records what the UI would receive. Each scenario is a list of calls; the result is one
ordered log of everything observable. `tests/pipeline.rs` runs the same scenarios against the Rust port.

Three more sections check the pieces on their own: flashtext's keyword matching (the real library, on random
keyword lists and sentences), `messageFormatter`, and the translation history.

The translator is scripted: a scenario's `translate` list is what the model's translator answers, call by
call (the generator fills it in when the scenario does not). The transliterator is a pure function of its
arguments (see `transliteration` below, mirrored in the Rust test).

This reads `src-python` and changes nothing in it. Run from anywhere:  python regenerate_pipeline_golden.py
"""

import ast
import copy
import dataclasses
import json
import random
import re
import sys
import time
from datetime import datetime
from pathlib import Path
from typing import Any, List, Optional

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[4]
SRC = REPO / "src-python"
sys.path.insert(0, str(SRC))

from flashtext import KeywordProcessor  # noqa: E402

from errors import ERROR_METADATA, ErrorCode, VRCTError  # noqa: E402
from models.message_pipeline import (  # noqa: E402
    CHAT_MESSAGE_SPEC, MIC_MESSAGE_SPEC, OCR_MESSAGE_SPEC, SPEAKER_MESSAGE_SPEC, MessageDirectionSpec,
)

EVENTS = []


def log_event(*event):
    EVENTS.append(list(event))


# ---- VRCT's own code, lifted out of controller.py and model.py -------------------------------------------

def lift_methods(path, class_name, names):
    source = (SRC / path).read_text(encoding="utf-8")
    lines = source.splitlines()
    tree = ast.parse(source)
    cls = next(n for n in tree.body if isinstance(n, ast.ClassDef) and n.name == class_name)
    pieces = {}
    for node in cls.body:
        if isinstance(node, ast.FunctionDef) and node.name in names:
            first = min([node.lineno] + [d.lineno for d in node.decorator_list])
            pieces[node.name] = "\n".join(lines[first - 1:node.end_lineno])
    assert set(pieces) == set(names), set(names) - set(pieces)
    return f"class Lifted:\n" + "\n\n".join(pieces[name] for name in names)


MAINLOOP = (SRC / "mainloop.py").read_text(encoding="utf-8")
RUN_MAPPING = next(
    ast.literal_eval(n.value) for n in ast.parse(MAINLOOP).body
    if isinstance(n, ast.Assign) and isinstance(n.targets[0], ast.Name) and n.targets[0].id == "run_mapping"
)


class FakeConfig:
    """`config`: attributes read from a dict; writes are logged."""

    def __init__(self, values):
        object.__setattr__(self, "_values", values)

    def __getattr__(self, name):
        try:
            return self._values[name]
        except KeyError:
            raise AttributeError(f"scenario does not define config.{name}")

    def __setattr__(self, name, value):
        self._values[name] = copy.deepcopy(value)
        log_event("config_set", name, copy.deepcopy(value))


def print_log(log, data=None):
    log_event("print", re.sub(r"\d+ms", "Nms", log))


CONTROLLER_NAMESPACE = {
    "Optional": Optional, "List": List, "Any": Any, "time": time, "VRCTError": VRCTError, "ErrorCode": ErrorCode,
    "MessageDirectionSpec": MessageDirectionSpec,
    "MIC_MESSAGE_SPEC": MIC_MESSAGE_SPEC, "SPEAKER_MESSAGE_SPEC": SPEAKER_MESSAGE_SPEC,
    "OCR_MESSAGE_SPEC": OCR_MESSAGE_SPEC, "CHAT_MESSAGE_SPEC": CHAT_MESSAGE_SPEC,
    "printLog": print_log, "errorLogging": lambda: None,
}
exec(
    compile(
        lift_methods("controller.py", "Controller", [
            "_is_overlay_available", "_processMessage", "micMessage", "speakerMessage", "ocrMessage",
            "_disableTranscriptionAfterPipelineError", "chatMessage", "messageFormatter",
        ]),
        "controller.py(lifted)", "exec",
    ),
    CONTROLLER_NAMESPACE,
)
LiftedController = CONTROLLER_NAMESPACE["Lifted"]

MODEL_NAMESPACE = {"datetime": datetime, "KeywordProcessor": KeywordProcessor}
exec(
    compile(
        lift_methods("model.py", "Model", [
            "addKeywords", "checkKeywords", "detectRepeatSendMessage", "detectRepeatReceiveMessage",
            "addTranslationHistory", "detectVRAMError",
        ]),
        "model.py(lifted)", "exec",
    ),
    MODEL_NAMESPACE,
)
LiftedModel = MODEL_NAMESPACE["Lifted"]


# ---- the fakes -------------------------------------------------------------------------------------------

def transliteration(message, hiragana, romaji):
    """Stands in for `Transliterator.analyze` + the key filter in `convertMessageToTransliteration`."""
    if hiragana is False and romaji is False:
        return []
    item = {"orig": message}
    if hiragana:
        item["hira"] = "h:" + message
    if romaji:
        item["hepburn"] = "r:" + message
    return [item]


class FakeLogger:
    def info(self, text):
        log_event("logger", text)


class FakeOverlay:
    def __init__(self, initialized):
        self.initialized = initialized


class FakeModel(LiftedModel):
    def __init__(self, config, scenario):
        self.config = config
        self.scenario = scenario
        self.translate_calls = 0
        self.mic_mute_status = scenario["mute"]
        self.previous_send_message = ""
        self.previous_receive_message = ""
        self.keyword_processor = KeywordProcessor()
        self.translation_history = []
        self.translation_history_max_items = 20
        self.logger = FakeLogger()
        self.overlay = {"none": None, "uninitialized": FakeOverlay(False), "ready": FakeOverlay(True)}[scenario["overlay"]]
        self._image = None
        self._images = 0
        for word in scenario["word_filter"]:
            self.keyword_processor.add_keyword(word)

    def ensure_initialized(self):
        pass

    def _translate(self, method, message, source_language):
        log_event("translate", method, message, source_language)
        index = self.translate_calls
        self.translate_calls += 1
        script = self.scenario["translate"]
        if index >= len(script):
            script.append(self.default_translation(method))
        response = script[index]
        if "raise" in response:
            kind, args = response["raise"]
            raise {"ValueError": ValueError, "RuntimeError": RuntimeError}[kind](*args)
        return list(response["ok"]), list(response["success"])

    def default_translation(self, method):
        return default_translation(self.config, method, self.scenario["_pending_message"])

    def getInputTranslate(self, message, source_language=None):
        return self._translate("input", message, source_language)

    def getOutputTranslate(self, message, source_language=None):
        return self._translate("output", message, source_language)

    def convertMessageToTransliteration(self, message, hiragana=True, romaji=True):
        log_event("transliterate", message, hiragana, romaji)
        return transliteration(message, hiragana, romaji)

    def oscSendMessage(self, message):
        log_event("osc", message)

    def _create_image(self, kind, args):
        self._images += 1
        self._image = f"{kind}#{self._images}"
        log_event("overlay_" + kind, args)
        return self._image

    def createOverlayImageSmallLog(self, *args):
        return self._create_image("small", list(args))

    def createOverlayImageLargeLog(self, *args):
        return self._create_image("large", list(args))

    def _update(self, kind, image):
        assert image == self._image and image.startswith(kind), (kind, image, self._image)

    def updateOverlaySmallLog(self, image):
        self._update("small", image)

    def updateOverlayLargeLog(self, image):
        self._update("large", image)

    def setCopyToClipboardAndPasteFromClipboard(self, text):
        log_event("clipboard", text)

    def checkWebSocketServerAlive(self):
        return self.scenario["ws_alive"]

    def websocketSendMessage(self, message):
        log_event("websocket", message)


def default_translation(config, method, message):
    if method == "output":
        return {"ok": [f"{message}>me"], "success": [True]}
    targets = [
        slot for slot, value in config["SELECTED_TARGET_LANGUAGES"][config["SELECTED_TAB_NO"]].items()
        if value["enable"] is True and (value["language"] is not None or value["country"] is not None)
    ]
    return {"ok": [f"{message}>{slot}" for slot in targets], "success": [True] * len(targets)}


class FakeController(LiftedController):
    def __init__(self, config, model):
        self.run_mapping = RUN_MAPPING
        self.config, self.model = config, model
        CONTROLLER_NAMESPACE["config"] = config
        CONTROLLER_NAMESPACE["model"] = model

    def run(self, status, endpoint, payload):
        log_event("run", status, endpoint, copy.deepcopy(payload))

    def setDisableTranslation(self):
        log_event("disable_translation")

    def changeToCTranslate2Process(self):
        log_event("fall_back_to_ctranslate2")


# ---- scenarios -------------------------------------------------------------------------------------------

def language(name, country, enable=True):
    return {"language": name, "country": country, "enable": enable}


def base_config():
    parts = lambda: {
        "message": {"prefix": "", "suffix": ""}, "separator": "\n",
        "translation": {"prefix": "", "separator": "\n", "suffix": ""}, "translation_first": False,
    }
    return {
        "ENABLE_TRANSLATION": False, "CONVERT_MESSAGE_TO_HIRAGANA": False, "CONVERT_MESSAGE_TO_ROMAJI": False,
        "SELECTED_TAB_NO": "1", "SELECTED_TAB_TARGET_LANGUAGES_NO_LIST": ["1", "2", "3"],
        "SELECTED_YOUR_LANGUAGES": {"1": {"1": language("Japanese", "Japan")}},
        "SELECTED_TARGET_LANGUAGES": {"1": {
            "1": language("English", "United States"),
            "2": language("Korean", "South Korea", False),
            "3": language("Japanese", "Japan", False),
        }},
        "ENABLE_TRANSCRIPTION_SEND": True, "ENABLE_TRANSCRIPTION_RECEIVE": True, "ENABLE_OCR_CAPTURE": True,
        "SEND_MESSAGE_TO_VRC": True, "SEND_RECEIVED_MESSAGE_TO_VRC": False, "SEND_ONLY_TRANSLATED_MESSAGES": False,
        "OVERLAY_SMALL_LOG": False, "OVERLAY_LARGE_LOG": False, "OVERLAY_SHOW_ONLY_TRANSLATED_MESSAGES": False,
        "ENABLE_CLIPBOARD": False, "LOGGER_FEATURE": False, "VRC_MIC_MUTE_SYNC": False,
        "SEND_MESSAGE_FORMAT_PARTS": parts(), "RECEIVED_MESSAGE_FORMAT_PARTS": parts(),
    }


def scenario(name, steps, config=None, **fields):
    values = base_config()
    for key, value in (config or {}).items():
        values[key] = value
    return {
        "name": name, "config": values, "mute": fields.get("mute"), "ws_alive": fields.get("ws_alive", False),
        "overlay": fields.get("overlay", "none"), "word_filter": fields.get("word_filter", []),
        "translate": fields.get("translate", []), "steps": steps,
    }


def mic(text="こんにちは", language="Japanese", **extra):
    return {"call": "mic", "arg": {"text": text, "language": language, **extra}}


def speaker(text="hello", language="English", **extra):
    return {"call": "speaker", "arg": {"text": text, "language": language, **extra}}


def ocr(text="hello", **extra):
    return {"call": "ocr", "arg": {"text": text, **extra}}


def chat(message="こんにちは", id="chat-1"):
    return {"call": "chat", "arg": {"id": id, "message": message}}


def error_result(**extra):
    return {"recognition_error": True, "text": "", "language": None, **extra}


PIPELINE_ERROR = {"error_code": "ASR_ERROR", "stage": "asr", "source": "mic", "message": "boom", "recoverable": False}

FORMAT_FANCY = {
    "message": {"prefix": "[", "suffix": "]"}, "separator": " | ",
    "translation": {"prefix": "<", "separator": " / ", "suffix": ">"}, "translation_first": True,
}


def handcrafted():
    translating = {"ENABLE_TRANSLATION": True}
    three = {"SELECTED_TARGET_LANGUAGES": {"1": {
        "1": language("English", "United States"), "2": language("Korean", "South Korea"),
        "3": language("Japanese", "Japan")}}}
    return [
        scenario("mic_plain", [mic()]),
        scenario("mic_translated", [mic()], translating),
        scenario("mic_three_targets", [mic()], {**translating, **three}),
        scenario("mic_three_targets_with_transliteration", [mic()], {**translating, **three, "CONVERT_MESSAGE_TO_HIRAGANA": True, "CONVERT_MESSAGE_TO_ROMAJI": True}),
        scenario("mic_transliteration_only_hiragana", [mic()], {"CONVERT_MESSAGE_TO_HIRAGANA": True}),
        scenario("mic_transliteration_romaji_english_speaker", [mic("hello", "English")], {"CONVERT_MESSAGE_TO_ROMAJI": True, "SELECTED_YOUR_LANGUAGES": {"1": {"1": language("English", "United States")}}}),
        scenario("mic_slot_index_quirk", [mic()], {
            "ENABLE_TRANSLATION": True, "CONVERT_MESSAGE_TO_HIRAGANA": True,
            "SELECTED_TARGET_LANGUAGES": {"1": {
                "1": language("English", "United States", False), "2": language("Japanese", "Japan"),
                "3": language("Korean", "South Korea", False)}}}),
        scenario("mic_no_target_enabled", [mic()], {"ENABLE_TRANSLATION": True, "SELECTED_TARGET_LANGUAGES": {"1": {
            "1": language("English", "United States", False), "2": language("Korean", "South Korea", False),
            "3": language("Japanese", "Japan", False)}}}),
        scenario("mic_word_filter_hit", [mic("this is a damn test")], word_filter=["damn"]),
        scenario("mic_word_filter_miss_then_hit", [mic("damnation"), mic("a DAMN thing"), mic("ok")], word_filter=["damn"]),
        scenario("mic_word_filter_japanese", [mic("これはバカです"), mic("こんにちは")], word_filter=["バカ"]),
        scenario("mic_repeat_is_dropped", [mic("same"), mic("same"), mic("other"), mic("same")]),
        scenario("mic_repeat_detector_is_updated_even_when_filtered", [mic("bad"), mic("bad")], word_filter=["bad"]),
        scenario("mic_osc_off", [mic()], {"SEND_MESSAGE_TO_VRC": False}),
        scenario("mic_send_only_translated", [mic()], {**translating, "SEND_ONLY_TRANSLATED_MESSAGES": True}),
        scenario("mic_send_only_translated_without_translation", [mic()], {"SEND_ONLY_TRANSLATED_MESSAGES": True}),
        scenario("mic_fancy_format", [mic()], {**translating, **three, "SEND_MESSAGE_FORMAT_PARTS": FORMAT_FANCY}),
        scenario("mic_clipboard", [mic()], {**translating, "ENABLE_CLIPBOARD": True}),
        scenario("mic_clipboard_uses_the_format_even_when_osc_is_off", [mic()], {**translating, "ENABLE_CLIPBOARD": True, "SEND_MESSAGE_TO_VRC": False, "SEND_MESSAGE_FORMAT_PARTS": FORMAT_FANCY}),
        scenario("mic_websocket", [mic()], {**translating, **three}, ws_alive=True),
        scenario("mic_logger", [mic()], {**translating, **three, "LOGGER_FEATURE": True}),
        scenario("mic_logger_without_translation", [mic()], {"LOGGER_FEATURE": True}),
        scenario("mic_transcription_send_off_skips_outputs", [mic()], {**translating, "ENABLE_TRANSCRIPTION_SEND": False, "ENABLE_CLIPBOARD": True, "LOGGER_FEATURE": True}, ws_alive=True),
        scenario("mic_overlay_large", [mic()], {**translating, "OVERLAY_LARGE_LOG": True}, overlay="ready"),
        scenario("mic_overlay_large_only_translated", [mic()], {**translating, "OVERLAY_LARGE_LOG": True, "OVERLAY_SHOW_ONLY_TRANSLATED_MESSAGES": True}, overlay="ready"),
        scenario("mic_overlay_large_only_translated_but_nothing_translated", [mic()], {"OVERLAY_LARGE_LOG": True, "OVERLAY_SHOW_ONLY_TRANSLATED_MESSAGES": True}, overlay="ready"),
        scenario("mic_overlay_not_initialised", [mic()], {**translating, "OVERLAY_LARGE_LOG": True, "OVERLAY_SMALL_LOG": True}, overlay="uninitialized"),
        scenario("mic_overlay_missing", [mic()], {**translating, "OVERLAY_LARGE_LOG": True}, overlay="none"),
        scenario("mic_small_overlay_is_for_speaker_only", [mic()], {**translating, "OVERLAY_SMALL_LOG": True}, overlay="ready"),
        scenario("mic_muted_by_vrchat", [mic(), mic("two")], {"VRC_MIC_MUTE_SYNC": True}, mute=True),
        scenario("mic_mute_sync_unmuted", [mic()], {"VRC_MIC_MUTE_SYNC": True}, mute=False),
        scenario("mic_mute_sync_unknown", [mic()], {"VRC_MIC_MUTE_SYNC": True}, mute=None),
        scenario("mic_muted_but_sync_off", [mic()], {"VRC_MIC_MUTE_SYNC": False}, mute=True),
        scenario("mic_recognition_error_is_reported_even_when_muted", [mic(**error_result())], {"VRC_MIC_MUTE_SYNC": True}, mute=True),
        scenario("mic_pipeline_error_turns_transcription_off", [mic(**error_result(**PIPELINE_ERROR))]),
        scenario("mic_pipeline_error_with_a_missing_key_is_a_network_error", [mic(**error_result(**{k: v for k, v in PIPELINE_ERROR.items() if k != "stage"}))]),
        scenario("mic_no_device", [mic(False, None)]),
        scenario("mic_empty_text", [mic("", None)]),
        scenario("mic_text_that_is_not_text", [mic(None, None), mic(5, None)]),
        scenario("mic_asr_ms_is_not_part_of_the_payload", [mic(asr_ms=120)]),
        scenario("mic_translation_failure_falls_back", [mic()], translating, translate=[{"ok": ["fallback"], "success": [False]}]),
        scenario("mic_partial_translation_failure", [mic()], {**translating, **three}, translate=[{"ok": ["a", "b", "c"], "success": [True, False, True]}]),
        scenario("mic_vram_error_with_a_message", [mic(), mic("again")], translating, translate=[{"raise": ["ValueError", ["VRAM_OUT_OF_MEMORY", "no memory left"]]}]),
        scenario("mic_vram_error_without_a_message", [mic()], translating, translate=[{"raise": ["ValueError", ["VRAM_OUT_OF_MEMORY"]]}]),
        scenario("mic_cuda_out_of_memory", [mic()], translating, translate=[{"raise": ["RuntimeError", ["CUDA out of memory. Tried to allocate 2 GiB"]]}]),
        scenario("mic_cublas_alloc_failed", [mic()], translating, translate=[{"raise": ["RuntimeError", ["CUBLAS_STATUS_ALLOC_FAILED"]]}]),
        scenario("mic_other_error_is_raised", [mic()], translating, translate=[{"raise": ["RuntimeError", ["connection reset"]]}]),
        scenario("mic_value_error_that_is_not_vram_is_raised", [mic()], translating, translate=[{"raise": ["ValueError", ["bad value"]]}]),
        scenario("speaker_plain", [speaker()]),
        scenario("speaker_translated", [speaker()], translating),
        scenario("speaker_osc_is_off_by_default", [speaker()], {**translating, "SEND_RECEIVED_MESSAGE_TO_VRC": False}),
        scenario("speaker_osc_on", [speaker()], {**translating, "SEND_RECEIVED_MESSAGE_TO_VRC": True}),
        scenario("speaker_osc_only_translated", [speaker()], {**translating, "SEND_RECEIVED_MESSAGE_TO_VRC": True, "SEND_ONLY_TRANSLATED_MESSAGES": True}),
        scenario("speaker_osc_only_translated_without_translation", [speaker()], {"SEND_RECEIVED_MESSAGE_TO_VRC": True, "SEND_ONLY_TRANSLATED_MESSAGES": True}),
        scenario("speaker_fancy_format", [speaker()], {**translating, "SEND_RECEIVED_MESSAGE_TO_VRC": True, "RECEIVED_MESSAGE_FORMAT_PARTS": FORMAT_FANCY}),
        scenario("speaker_transliteration_of_a_japanese_speaker", [speaker("こんにちは", "Japanese")], {**translating, "CONVERT_MESSAGE_TO_HIRAGANA": True, "CONVERT_MESSAGE_TO_ROMAJI": True}),
        scenario("speaker_transliteration_of_english_to_japanese", [speaker()], {**translating, "CONVERT_MESSAGE_TO_HIRAGANA": True}),
        scenario("speaker_transliteration_without_translation", [speaker("こんにちは", "Japanese")], {"CONVERT_MESSAGE_TO_ROMAJI": True}),
        scenario("speaker_overlays", [speaker()], {**translating, "OVERLAY_SMALL_LOG": True, "OVERLAY_LARGE_LOG": True}, overlay="ready"),
        scenario("speaker_overlays_only_translated", [speaker()], {**translating, "OVERLAY_SMALL_LOG": True, "OVERLAY_LARGE_LOG": True, "OVERLAY_SHOW_ONLY_TRANSLATED_MESSAGES": True}, overlay="ready"),
        scenario("speaker_overlays_only_translated_but_nothing_translated", [speaker()], {"OVERLAY_SMALL_LOG": True, "OVERLAY_LARGE_LOG": True, "OVERLAY_SHOW_ONLY_TRANSLATED_MESSAGES": True}, overlay="ready"),
        scenario("speaker_overlays_not_initialised", [speaker()], {**translating, "OVERLAY_SMALL_LOG": True, "OVERLAY_LARGE_LOG": True}, overlay="uninitialized"),
        scenario("speaker_clipboard_is_for_mic_only", [speaker()], {**translating, "ENABLE_CLIPBOARD": True}),
        scenario("speaker_websocket", [speaker()], translating, ws_alive=True),
        scenario("speaker_logger", [speaker()], {**translating, "LOGGER_FEATURE": True}),
        scenario("speaker_transcription_receive_off", [speaker()], {**translating, "ENABLE_TRANSCRIPTION_RECEIVE": False, "LOGGER_FEATURE": True}, ws_alive=True),
        scenario("speaker_word_filter_and_repeats_are_kept_apart_from_mic", [mic("same"), speaker("same"), speaker("same"), mic("same")], word_filter=["zzz"]),
        scenario("speaker_word_filter", [speaker("zzz here")], word_filter=["zzz"]),
        scenario("speaker_recognition_error", [speaker(**error_result())]),
        scenario("speaker_pipeline_error", [speaker(**error_result(**{**PIPELINE_ERROR, "source": "speaker"}))]),
        scenario("speaker_no_device", [speaker(False, None)]),
        scenario("speaker_translation_failure_falls_back", [speaker()], translating, translate=[{"ok": ["x"], "success": [False]}]),
        scenario("speaker_vram_error", [speaker()], translating, translate=[{"raise": ["ValueError", ["VRAM_OUT_OF_MEMORY", "low"]]}]),
        scenario("ocr_plain", [ocr()], {"ENABLE_OCR_CAPTURE": True}),
        scenario("ocr_translated_with_language", [ocr("こんにちは", language="Japanese", segment_id=7)], translating),
        scenario("ocr_empty_language_is_no_language", [ocr("hello", language="", segment_id=8)], translating),
        scenario("ocr_without_segment_id", [ocr("hello")], translating),
        scenario("ocr_never_sends_osc", [ocr("hello", segment_id=1)], {**translating, "SEND_RECEIVED_MESSAGE_TO_VRC": True, "SEND_MESSAGE_TO_VRC": True}),
        scenario("ocr_large_overlay_only", [ocr("hello", segment_id=2)], {**translating, "OVERLAY_SMALL_LOG": True, "OVERLAY_LARGE_LOG": True}, overlay="ready"),
        scenario("ocr_is_off", [ocr("hello", segment_id=3)], {"ENABLE_OCR_CAPTURE": False, "LOGGER_FEATURE": True}, ws_alive=True),
        scenario("ocr_no_text", [ocr(None), ocr(""), {"call": "ocr", "arg": {}}]),
        scenario("ocr_word_filter_but_no_repeat_filter", [ocr("bad", segment_id=1), ocr("fine", segment_id=2), ocr("fine", segment_id=3)], word_filter=["bad"]),
        scenario("ocr_websocket_and_logger", [ocr("hello", segment_id=4)], {**translating, "LOGGER_FEATURE": True}, ws_alive=True),
        scenario("ocr_vram_error", [ocr("hello", segment_id=5)], translating, translate=[{"raise": ["ValueError", ["VRAM_OUT_OF_MEMORY", "oom"]]}]),
        scenario("chat_plain", [chat()]),
        scenario("chat_translated", [chat()], translating),
        scenario("chat_three_targets_with_transliteration", [chat()], {**translating, **three, "CONVERT_MESSAGE_TO_HIRAGANA": True}),
        scenario("chat_empty_message", [chat("")]),
        scenario("chat_is_not_word_filtered_nor_repeat_filtered", [chat("damn"), chat("damn")], word_filter=["damn"]),
        scenario("chat_osc_off", [chat()], {"SEND_MESSAGE_TO_VRC": False}),
        scenario("chat_overlay_large", [chat()], {**translating, "OVERLAY_LARGE_LOG": True}, overlay="ready"),
        scenario("chat_overlay_not_initialised_is_skipped", [chat()], {**translating, "OVERLAY_LARGE_LOG": True}, overlay="uninitialized"),
        scenario("chat_clipboard_is_for_mic_only", [chat()], {**translating, "ENABLE_CLIPBOARD": True}),
        scenario("chat_websocket_and_logger", [chat()], {**translating, "LOGGER_FEATURE": True}, ws_alive=True),
        scenario("chat_translation_failure_falls_back", [chat()], translating, translate=[{"ok": ["x"], "success": [False]}]),
        scenario("chat_vram_error_returns_the_original", [chat()], {**translating, **three}, translate=[{"raise": ["ValueError", ["VRAM_OUT_OF_MEMORY", "oom"]]}]),
        scenario("chat_other_error_is_raised", [chat()], translating, translate=[{"raise": ["RuntimeError", ["down"]]}]),
        scenario("chat_history_is_shared", [mic("one"), speaker("two"), chat("three"), ocr("four", segment_id=1)], translating),
        scenario("history_keeps_twenty", [mic(f"message {i}") for i in range(23)]),
        scenario("history_ignores_blank_messages", [chat(" "), chat("  hi  ")]),
        scenario("history_is_not_written_when_filtered", [mic("bad"), mic("ok")], word_filter=["bad"]),
    ]


MESSAGES = ["こんにちは", "hello world", "Hello", "テスト", "damn it", "ありがとう", "good morning", "  ", "a", "漢字"]
LANGUAGES = [None, "Japanese", "English", "Korean"]
YOURS = [("Japanese", "Japan"), ("English", "United States"), ("Korean", "South Korea")]


def random_scenarios(count, rng):
    result = []
    for number in range(count):
        flag = lambda p=0.5: rng.random() < p
        yours = rng.choice(YOURS)
        targets = {
            slot: language(*rng.choice(YOURS + [("English", "United States")]), flag(0.5)) for slot in ("1", "2", "3")
        }
        parts = lambda: {
            "message": {"prefix": rng.choice(["", "[", "「"]), "suffix": rng.choice(["", "]", "」"])},
            "separator": rng.choice(["\n", " | ", ""]),
            "translation": {"prefix": rng.choice(["", "<"]), "separator": rng.choice(["\n", " / "]), "suffix": rng.choice(["", ">"])},
            "translation_first": flag(),
        }
        config = {
            "ENABLE_TRANSLATION": flag(0.7), "CONVERT_MESSAGE_TO_HIRAGANA": flag(0.3), "CONVERT_MESSAGE_TO_ROMAJI": flag(0.3),
            "SELECTED_YOUR_LANGUAGES": {"1": {"1": language(*yours)}}, "SELECTED_TARGET_LANGUAGES": {"1": targets},
            "ENABLE_TRANSCRIPTION_SEND": flag(0.85), "ENABLE_TRANSCRIPTION_RECEIVE": flag(0.85), "ENABLE_OCR_CAPTURE": flag(0.9),
            "SEND_MESSAGE_TO_VRC": flag(0.8), "SEND_RECEIVED_MESSAGE_TO_VRC": flag(0.5), "SEND_ONLY_TRANSLATED_MESSAGES": flag(0.3),
            "OVERLAY_SMALL_LOG": flag(), "OVERLAY_LARGE_LOG": flag(), "OVERLAY_SHOW_ONLY_TRANSLATED_MESSAGES": flag(0.4),
            "ENABLE_CLIPBOARD": flag(0.4), "LOGGER_FEATURE": flag(0.4), "VRC_MIC_MUTE_SYNC": flag(0.3),
            "SEND_MESSAGE_FORMAT_PARTS": parts(), "RECEIVED_MESSAGE_FORMAT_PARTS": parts(),
        }
        steps = []
        for _ in range(rng.randint(1, 5)):
            kind = rng.choice(["mic", "mic", "speaker", "speaker", "ocr", "chat"])
            text = rng.choice(MESSAGES)
            if kind == "mic":
                step = mic(text, rng.choice(LANGUAGES), **({"asr_ms": rng.randint(1, 900)} if flag(0.3) else {}))
            elif kind == "speaker":
                step = speaker(text, rng.choice(LANGUAGES))
            elif kind == "ocr":
                step = ocr(text, language=rng.choice(LANGUAGES + [""]), **({"segment_id": rng.randint(0, 99)} if flag(0.7) else {}))
            else:
                step = chat(text, f"chat-{rng.randint(0, 99)}")
            steps.append(step)
        script = []
        for _ in range(len(steps)):
            roll = rng.random()
            if roll < 0.6:
                script.append(None)
            elif roll < 0.8:
                script.append("partial_failure")
            elif roll < 0.9:
                script.append(["ValueError", ["VRAM_OUT_OF_MEMORY", "oom"]])
            else:
                script.append(["RuntimeError", ["CUDA out of memory"]])
        item = scenario(
            f"random_{number:03d}", steps, config, mute=rng.choice([None, True, False]), ws_alive=flag(0.5),
            overlay=rng.choice(["none", "uninitialized", "ready", "ready"]),
            word_filter=rng.choice([[], [], ["damn"], ["テス"], ["hello"]]),
        )
        item["_plan"] = script
        result.append(item)
    return result


# ---- running a scenario ----------------------------------------------------------------------------------

def run_scenario(item):
    plan = item.pop("_plan", [])
    del EVENTS[:]
    config = FakeConfig(copy.deepcopy(item["config"]))
    item["translate"] = list(item["translate"])
    model = FakeModel(config._values, item)
    controller = FakeController(config, model)
    explicit = len(item["translate"])
    for step in item["steps"]:
        item["_pending_message"] = step["arg"].get("text") if step["call"] != "chat" else step["arg"]["message"]
        log_event("call", step["call"])
        arg = copy.deepcopy(step["arg"])
        # A random plan decides how this step's translator call (if it makes one) answers.
        if explicit == 0 and plan:
            choice = plan.pop(0)
            if choice is not None and len(item["translate"]) == model.translate_calls:
                method = "output" if step["call"] in ("speaker", "ocr") else "input"
                response = default_translation(config._values, method, item["_pending_message"] or "")
                if choice == "partial_failure":
                    if response["success"]:
                        response["success"][0] = False
                    item["translate"].append(response)
                else:
                    item["translate"].append({"raise": choice})
        try:
            if step["call"] == "mic":
                value = controller.micMessage(arg)
            elif step["call"] == "speaker":
                value = controller.speakerMessage(arg)
            elif step["call"] == "ocr":
                value = controller.ocrMessage(arg)
            else:
                value = controller.chatMessage(arg)
            log_event("returned", value)
        except (IndexError, ValueError, RuntimeError) as error:
            category = "index" if isinstance(error, IndexError) else "translate"
            log_event("raised", category, str(error) if category == "translate" else None)
        log_event("history", [[h["source"], h["text"]] for h in model.translation_history])
        for entry in model.translation_history:
            assert re.fullmatch(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(\.\d{6})?", entry["timestamp"]), entry["timestamp"]
    item.pop("_pending_message", None)
    item["events"] = [list(e) for e in EVENTS]
    return item


# ---- the small pieces ------------------------------------------------------------------------------------

def keyword_cases(rng):
    pool = list("abcXYZ019_ .,-!") + ["あ", "い", "カ", "漢", "字", "É", "é", "İ", "Σ", "ς", "ß", "😀", "'", "\t", "\n"]
    cases = []
    for _ in range(600):
        keywords = ["".join(rng.choice(pool) for _ in range(rng.randint(0, 5))) for _ in range(rng.randint(0, 4))]
        if rng.random() < 0.5 and keywords:
            # words that really occur: build the sentence out of keywords and glue
            sentence = "".join(rng.choice(keywords + [" ", ".", "あ", "x"]) for _ in range(rng.randint(0, 6)))
        else:
            sentence = "".join(rng.choice(pool) for _ in range(rng.randint(0, 24)))
        processor = KeywordProcessor()
        for word in keywords:
            processor.add_keyword(word)
        cases.append({"keywords": keywords, "sentence": sentence, "found": processor.extract_keywords(sentence)})
    for keywords, sentence in [
        (["damn"], "damn"), (["damn"], "damn!"), (["damn"], "a damn b"), (["damn"], "damnit"), (["damn"], "adamn"),
        (["バカ"], "あバカい"), (["big apple", "big"], "a big apple pie"), (["Big"], "BIG"), ([""], "abc"), ([], "abc"),
        (["a b"], "a  b"), (["a_b"], "a_b c"), (["x"], ""), (["日本"], "日本語"),
    ]:
        processor = KeywordProcessor()
        for word in keywords:
            processor.add_keyword(word)
        cases.append({"keywords": keywords, "sentence": sentence, "found": processor.extract_keywords(sentence)})
    return cases


def formatter_cases(rng):
    cases = []
    for number in range(300):
        parts = {
            "message": {"prefix": rng.choice(["", "[", "「"]), "suffix": rng.choice(["", "]", "」"])},
            "separator": rng.choice(["\n", " | ", ""]),
            "translation": {"prefix": rng.choice(["", "<"]), "separator": rng.choice(["\n", " / ", ""]), "suffix": rng.choice(["", ">"])},
            "translation_first": rng.random() < 0.5,
        }
        translation = [rng.choice(["a", "b b", "ほげ", ""]) for _ in range(rng.choice([0, 0, 1, 2, 3]))]
        message = rng.choice(["", "hello", "こんにちは"])
        kind = rng.choice(["SEND", "RECEIVED"])
        config = FakeConfig({"SEND_MESSAGE_FORMAT_PARTS": parts, "RECEIVED_MESSAGE_FORMAT_PARTS": copy.deepcopy(parts)})
        CONTROLLER_NAMESPACE["config"] = config
        cases.append({
            "parts": parts, "kind": kind, "translation": translation, "message": message,
            "expected": LiftedController.messageFormatter(kind, translation, message),
        })
    CONTROLLER_NAMESPACE["config"] = FakeConfig({"SEND_MESSAGE_FORMAT_PARTS": {}, "RECEIVED_MESSAGE_FORMAT_PARTS": {}})
    try:
        LiftedController.messageFormatter("OTHER", [], "x")
    except ValueError:
        cases.append({"parts": None, "kind": "OTHER", "translation": [], "message": "x", "expected": None})
    return cases


def vram_cases():
    class Probe(LiftedModel):
        pass

    probe = Probe()
    errors = [
        ValueError("VRAM_OUT_OF_MEMORY", "m"), ValueError("VRAM_OUT_OF_MEMORY"), ValueError("other"),
        RuntimeError("CUDA out of memory. x"), RuntimeError("xx CUBLAS_STATUS_ALLOC_FAILED"), RuntimeError("VRAM_OUT_OF_MEMORY"),
        RuntimeError("boom"),
    ]
    return [
        {"error": [type(e).__name__, [str(a) for a in e.args]], "result": list(probe.detectVRAMError(e))} for e in errors
    ]


def main():
    rng = random.Random(20261003)
    scenarios = [run_scenario(item) for item in handcrafted() + random_scenarios(100, rng)]
    base = base_config()
    for item in scenarios:
        item["config"] = {key: value for key, value in item["config"].items() if base[key] != value}
    out = {
        "base_config": base,
        "specs": {
            spec.kind: {
                key: (value.value if isinstance(value, ErrorCode) else value)
                for key, value in dataclasses.asdict(spec).items()
            }
            for spec in (MIC_MESSAGE_SPEC, SPEAKER_MESSAGE_SPEC, OCR_MESSAGE_SPEC, CHAT_MESSAGE_SPEC)
        },
        "run_mapping": {k: v for k, v in RUN_MAPPING.items()},
        "scenarios": scenarios,
        "keywords": keyword_cases(random.Random(7)),
        "formatter": formatter_cases(random.Random(11)),
        "vram": vram_cases(),
        "errors": {
            code.value: {
                "message": ERROR_METADATA[code]["message"], "category": ERROR_METADATA[code]["category"].value,
                "severity": ERROR_METADATA[code]["severity"],
            }
            for code in (
                ErrorCode.TRANSLATION_ENGINE_LIMIT, ErrorCode.TRANSLATION_VRAM_MIC, ErrorCode.TRANSLATION_VRAM_SPEAKER,
                ErrorCode.TRANSLATION_VRAM_CHAT, ErrorCode.TRANSLATION_DISABLED_VRAM,
            )
        },
    }
    path = HERE / "pipeline_golden.json"
    path.write_text(json.dumps(out, ensure_ascii=False, indent=1) + "\n", encoding="utf-8")
    kinds = {}
    for item in scenarios:
        for event in item["events"]:
            kinds[event[0]] = kinds.get(event[0], 0) + 1
    print(f"{len(scenarios)} scenarios, {len(out['keywords'])} keyword cases, {len(out['formatter'])} formats", kinds)


if __name__ == "__main__":
    main()
