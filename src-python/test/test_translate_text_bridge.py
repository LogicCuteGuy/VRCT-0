"""`translate.text` (翻訳全体の Rust 委譲) に関するテスト。

ホストが VRCT_RUST_RPC に "translate.text" を載せて起動した場合、DeepL と LLM 系エンジンは
言語名のままホストに渡し、コード解決・サポート外の判定・HTTP 呼び出しをホストが行う。
Python が持つのは認証済みの鍵・モデル・URL・会話履歴だけ。ホストが担当しないエンジンや
載っていない場合は従来の経路。
"""

import os
import unittest
from unittest.mock import MagicMock, patch

import utils
from models.translation import translation_translator as tt
from models.translation.translation_languages import loadTranslationLanguages
from models.translation.translation_translator import Translator
from test_translate_llm_bridge import _FakeClient, _Host

loadTranslationLanguages(path=os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

_ENGINES = ["OpenAI_API", "Gemini_API", "Groq_API", "OpenRouter_API", "Plamo_API",
            "OpenAI_Compatible", "LMStudio", "Ollama"]


class TranslateTextBridgeTests(unittest.TestCase):
    def setUp(self) -> None:
        self.translator = Translator()
        self.translator.is_enable_translators = True
        self.host = _Host(answer=lambda request: {"ok": True, "result": {"kind": "text", "text": "rust:" + request["params"]["text"]}})
        patcher = patch.object(utils, "_enqueueResponseLine", self.host)
        patcher.start()
        self.addCleanup(patcher.stop)
        env = patch.dict(os.environ, {utils._RPC_ENV: "translate.text"})
        env.start()
        self.addCleanup(env.stop)

    def _translate(self, engine, **overrides):
        args = dict(
            translator_name=engine,
            weight_type="",
            source_language="Japanese",
            target_language="English",
            target_country="United States",
            message="こんにちは",
        )
        args.update(overrides)
        return self.translator.translate(**args)

    def _use_client(self, engine, client):
        if engine == "OpenAI_Compatible":
            self.translator.openai_compatible_client = client
        elif engine == "LMStudio":
            self.translator.lmstudio_client = client
        elif engine == "Ollama":
            self.translator.ollama_client = client
        else:
            self.translator._provider_clients[engine] = client

    def test_every_hosted_engine_is_sent_language_names_and_its_own_state(self) -> None:
        for engine in _ENGINES:
            with self.subTest(engine=engine):
                client = _FakeClient(api_key="sk-" + engine, model="model-" + engine, base_url="http://x/" + engine)
                self._use_client(engine, client)
                self.host.requests.clear()
                self.assertEqual(self._translate(engine, weight_type="w", target_country="Japan"), "rust:こんにちは")
                self.assertEqual(client.calls, [], f"{engine} must not run the Python client")
                (request,) = self.host.requests
                self.assertEqual(request["method"], "translate.text")
                self.assertEqual(request["params"], {
                    "engine": engine,
                    "source_language": "Japanese",
                    "target_language": "English",
                    "target_country": "Japan",
                    "weight_type": "w",
                    "text": "こんにちは",
                    "api_key": "sk-" + engine,
                    "base_url": "http://x/" + engine,
                    "model": "model-" + engine,
                    "history": [],
                })

    def test_history_is_set_on_the_client_and_sent(self) -> None:
        history = [{"source": "chat", "text": "hi", "timestamp": "2026-10-02T10:00:00"}]
        client = _FakeClient()
        self._use_client("OpenAI_API", client)
        self._translate("OpenAI_API", context_history=history)
        self._translate("OpenAI_API")  # an empty history does not reset it
        self.assertEqual([r["params"]["history"] for r in self.host.requests], [history, history])

    def test_unsupported_means_none_and_failure_means_false(self) -> None:
        self._use_client("OpenAI_API", _FakeClient())
        self.host.answer = lambda request: {"ok": True, "result": {"kind": "unsupported", "reason": "x"}}
        self.assertIsNone(self._translate("OpenAI_API"))
        self.host.answer = lambda request: {"ok": False, "error": "HTTP 401: bad key"}
        with patch("models.translation.translation_translator.errorLogging"):
            self.assertIs(self._translate("OpenAI_API"), False)

    def test_an_unconfigured_engine_fails_without_calling_the_host(self) -> None:
        for engine in _ENGINES:
            self.assertIs(self._translate(engine), False, engine)
        self.assertEqual(self.host.requests, [])

    def test_the_same_language_is_returned_untouched(self) -> None:
        self._use_client("OpenAI_API", _FakeClient())
        self.assertEqual(self._translate("OpenAI_API", target_language="Japanese"), "こんにちは")
        self.assertEqual(self.host.requests, [])

    def test_translate_text_wins_over_the_lower_level_method(self) -> None:
        self._use_client("OpenAI_API", _FakeClient())
        with patch.dict(os.environ, {utils._RPC_ENV: "translate.text,translate.llm"}):
            self._translate("OpenAI_API")
        self.assertEqual([r["method"] for r in self.host.requests], ["translate.text"])

    def test_deepl_sends_its_key_and_the_country(self) -> None:
        self.translator.deepl_client = tt._HostDeepLClient("secret:fx")
        self.assertEqual(self._translate("DeepL_API", target_country="Canada"), "rust:こんにちは")
        (request,) = self.host.requests
        self.assertEqual(request["params"]["api_key"], "secret:fx")
        self.assertEqual(request["params"]["target_country"], "Canada")
        self.assertEqual(request["params"]["history"], [])

    def test_deepl_without_a_key_or_translators_behaves_as_before(self) -> None:
        self.assertIs(self._translate("DeepL_API"), False)
        self.translator.deepl_client = tt._HostDeepLClient("k")
        self.translator.is_enable_translators = False
        self.assertEqual(self._translate("DeepL_API"), "")
        self.assertEqual(self.host.requests, [])

    def test_a_deepl_sdk_client_keeps_using_python(self) -> None:
        sdk = MagicMock()
        sdk.translate_text.return_value.text = "sdk:hello"
        self.translator.deepl_client = sdk
        self.assertEqual(self._translate("DeepL_API"), "sdk:hello")
        self.assertEqual(self.host.requests, [])

    def test_engines_the_host_does_not_perform_stay_in_python(self) -> None:
        with patch.object(tt, "other_web_Translator", return_value="web:hello") as web, patch.object(tt, "ENABLE_TRANSLATORS", True):
            self.assertEqual(self._translate("Google"), "web:hello")
        web.assert_called_once()
        self.assertEqual(self.host.requests, [])

    def test_without_the_method_python_does_everything_as_before(self) -> None:
        for value in (None, "", "translate.llm"):
            with self.subTest(value=value), patch.dict(os.environ, clear=False):
                os.environ.pop(utils._RPC_ENV, None)
                if value is not None:
                    os.environ[utils._RPC_ENV] = value
                client = _FakeClient()
                self._use_client("OpenAI_API", client)
                self.host.requests.clear()
                self.host.answer = lambda request: {"ok": True, "result": "llm:" + request["params"]["text"]}
                result = self._translate("OpenAI_API")
                self.assertEqual(result, "llm:こんにちは" if value == "translate.llm" else "python:こんにちは")
                self.assertTrue(all(r["method"] == "translate.llm" for r in self.host.requests))


if __name__ == "__main__":
    unittest.main()
