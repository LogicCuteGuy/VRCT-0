"""ローカルモデル (CTranslate2) の読み込みと翻訳のホスト委譲 ("ct2.load" / "ct2.translate") のテスト。

ホストが VRCT_RUST_RPC に載せて起動した場合、モデルはホスト (Rust) が読み込んで保持し、
Python 側の translator / tokenizer は使わない。ホストが断った (CUDA、NLLB など) か
読み込みに失敗した場合は、従来どおり Python が読み込む。
"""

import os
import unittest
from unittest.mock import MagicMock, patch

import utils
from models.translation import translation_translator as tt
from models.translation.translation_translator import Translator
from test_translate_llm_bridge import _Host

_WEIGHT = "m2m100_418M-ct2-int8"


def _answer(request):
    if request["method"] == "ct2.load":
        return {"ok": True, "result": True}
    return {"ok": True, "result": "rust:" + request["params"]["message"]}


class CTranslate2BridgeTests(unittest.TestCase):
    def setUp(self) -> None:
        self.translator = Translator()
        self.host = _Host(answer=_answer)
        patcher = patch.object(utils, "_enqueueResponseLine", self.host)
        patcher.start()
        self.addCleanup(patcher.stop)
        self.enable("ct2.load,ct2.translate")
        # Python のライブラリの代役: ホストが断ったときにだけ使われる。
        self.python_ct2 = MagicMock(name="ctranslate2")
        self.python_tf = MagicMock(name="transformers")
        for name, fake in (("ctranslate2", self.python_ct2), ("transformers", self.python_tf)):
            patch_ = patch.object(tt, name, fake)
            patch_.start()
            self.addCleanup(patch_.stop)
        best = patch.object(tt, "getBestComputeType", return_value="int8_float32")
        self.best = best.start()
        self.addCleanup(best.stop)

    def enable(self, methods: str) -> None:
        env = patch.dict(os.environ, {utils._RPC_ENV: methods})
        env.start()
        self.addCleanup(env.stop)

    def _load(self, **overrides):
        args = dict(path="C:/vrct", model_type=_WEIGHT, device="cpu", device_index=0, compute_type="auto")
        args.update(overrides)
        self.translator.changeCTranslate2Model(**args)

    def _translate(self, message="こんにちは"):
        return self.translator.translateCTranslate2(message, "ja", "en", _WEIGHT)

    def test_the_host_loads_the_model_and_python_loads_nothing(self) -> None:
        self._load()
        (request,) = self.host.requests
        self.assertEqual(request["method"], "ct2.load")
        self.assertEqual(request["params"], {
            "path": "C:/vrct", "weight_type": _WEIGHT, "device": "cpu",
            "device_index": 0, "compute_type": "int8_float32",
        })
        self.best.assert_called_once_with("cpu", 0)
        self.assertTrue(self.translator.isLoadedCTranslate2Model())
        self.assertTrue(self.translator.ctranslate2_in_host)
        self.python_ct2.Translator.assert_not_called()
        self.python_tf.AutoTokenizer.from_pretrained.assert_not_called()

    def test_an_explicit_compute_type_is_passed_as_it_is(self) -> None:
        self._load(compute_type="float32", device_index=2)
        self.assertEqual(self.host.requests[0]["params"]["compute_type"], "float32")
        self.assertEqual(self.host.requests[0]["params"]["device_index"], 2)
        self.best.assert_not_called()

    def test_translation_goes_to_the_host_with_the_resolved_codes(self) -> None:
        self._load()
        self.host.requests.clear()
        self.assertEqual(self._translate("おはよう"), "rust:おはよう")
        (request,) = self.host.requests
        self.assertEqual(request["method"], "ct2.translate")
        self.assertEqual(request["params"], {
            "message": "おはよう", "source_language": "ja", "target_language": "en", "weight_type": _WEIGHT,
        })
        self.python_tf.AutoTokenizer.from_pretrained.assert_not_called()

    def test_a_host_failure_while_translating_is_the_legacy_false(self) -> None:
        self._load()
        self.host.answer = lambda request: {"ok": False, "error": "translation failed: boom"}
        with patch.object(tt, "errorLogging") as log:
            self.assertIs(self._translate(), False)
        log.assert_called_once()

    def test_a_refusal_falls_back_to_loading_in_python(self) -> None:
        self.host.answer = lambda request: {"ok": False, "error": "device \"cuda\" is not supported by this build (CPU only)"}
        self._load(device="cuda")
        self.assertTrue(self.translator.isLoadedCTranslate2Model())
        self.assertFalse(self.translator.ctranslate2_in_host)
        self.python_ct2.Translator.assert_called_once()
        self.python_tf.AutoTokenizer.from_pretrained.assert_called_once()
        # ... and translation then runs in Python, not in the host.
        self.host.requests.clear()
        tokenizer = self.translator.ctranslate2_tokenizer
        tokenizer.lang_code_to_token = {"en": "__en__"}
        tokenizer.convert_ids_to_tokens.return_value = ["__ja__", "x"]
        self.translator.ctranslate2_translator.translate_batch.return_value = [MagicMock(hypotheses=[["__en__", "y"]])]
        tokenizer.decode.return_value = "python"
        self.assertEqual(self._translate(), "python")
        self.assertEqual(self.host.requests, [])

    def test_a_host_that_answers_false_is_treated_as_a_refusal(self) -> None:
        self.host.answer = lambda request: {"ok": True, "result": False}
        self._load()
        self.assertFalse(self.translator.ctranslate2_in_host)
        self.python_ct2.Translator.assert_called_once()

    def test_a_refusal_with_no_python_libraries_leaves_nothing_loaded(self) -> None:
        self.host.answer = lambda request: {"ok": False, "error": "no tokenizer files"}
        with patch.object(tt, "ctranslate2", None):
            self._load()
        self.assertFalse(self.translator.isLoadedCTranslate2Model())
        self.assertFalse(self.translator.ctranslate2_in_host)
        self.assertIs(self._translate(), False)

    def test_a_python_load_after_a_host_load_does_not_keep_the_old_flag(self) -> None:
        self._load()
        self.assertTrue(self.translator.ctranslate2_in_host)
        self.host.answer = lambda request: {"ok": False, "error": "refused"}
        self._load(device="cuda")
        self.assertFalse(self.translator.ctranslate2_in_host)
        self.assertTrue(self.translator.isLoadedCTranslate2Model())

    def test_a_failed_reload_drops_the_previous_host_model(self) -> None:
        self._load()
        self.host.answer = lambda request: {"ok": False, "error": "refused"}
        with patch.object(tt, "ctranslate2", None):
            self._load(device="cuda")
        self.assertFalse(self.translator.isLoadedCTranslate2Model())
        self.assertFalse(self.translator.ctranslate2_in_host)

    def test_translating_with_nothing_loaded_sends_nothing(self) -> None:
        self.assertIs(self._translate(), False)
        self.assertEqual(self.host.requests, [])

    def test_without_the_switch_everything_stays_in_python(self) -> None:
        with patch.dict(os.environ, {utils._RPC_ENV: "translate.text"}):
            self._load()
        self.assertEqual(self.host.requests, [])
        self.assertFalse(self.translator.ctranslate2_in_host)
        self.python_ct2.Translator.assert_called_once()

    def test_a_model_the_host_holds_is_translated_by_the_host(self) -> None:
        # The model lives wherever it was loaded, whatever the env says afterwards.
        with patch.dict(os.environ, {utils._RPC_ENV: "ct2.load"}):
            self._load()
            self.assertTrue(self.translator.ctranslate2_in_host)
            self.host.requests.clear()
            self.assertEqual(self._translate(), "rust:こんにちは")
        self.assertEqual(self.host.requests[0]["method"], "ct2.translate")


if __name__ == "__main__":
    unittest.main()
