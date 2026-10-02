"""LLM エンジンの認証確認・モデル一覧の Rust 委譲 (RPC bridge) に関するテスト。

ホストが VRCT_RUST_RPC に "llm.auth_check" / "llm.models" を載せて起動した場合だけ、
各クライアントモジュールの `_authentication_check` / `_get_available_text_models` は
ホストに問い合わせる。結果の扱い (例外を握りつぶして False/[] にするか、そのまま伝えるか)
は従来の関数のまま。載っていなければ従来通り SDK / requests を使う。
"""

import os
import unittest
from unittest.mock import MagicMock, patch

import utils
from models.translation import (
    translation_gemini,
    translation_groq,
    translation_lmstudio,
    translation_ollama,
    translation_openai,
    translation_openai_compatible,
    translation_openrouter,
    translation_plamo,
)
from models.translation.translation_languages import loadTranslationLanguages
from test_translate_llm_bridge import _Host

loadTranslationLanguages(path=os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

_BOTH = "llm.auth_check,llm.models"

# (module, engine, extra positional args for the module-level functions)
_KEYED = [
    (translation_openai, "OpenAI_API", ("sk-1", "http://x/v1")),
    (translation_gemini, "Gemini_API", ("g-1",)),
    (translation_groq, "Groq_API", ("gsk-1",)),
    (translation_plamo, "Plamo_API", ("p-1",)),
]


class CatalogBridgeTests(unittest.TestCase):
    def setUp(self) -> None:
        self.host = _Host(answer=self._answer)
        self.replies = {"llm.auth_check": True, "llm.models": ["m-b", "m-a"]}
        patcher = patch.object(utils, "_enqueueResponseLine", self.host)
        patcher.start()
        self.addCleanup(patcher.stop)
        env = patch.dict(os.environ, {utils._RPC_ENV: _BOTH})
        env.start()
        self.addCleanup(env.stop)

    def _answer(self, request):
        reply = self.replies[request["method"]]
        if isinstance(reply, Exception):
            return {"ok": False, "error": str(reply)}
        return {"ok": True, "result": reply}

    def _params(self):
        return [(r["method"], r["params"]) for r in self.host.requests]

    def test_keyed_engines_ask_the_host_with_their_own_name_and_key(self) -> None:
        for module, engine, args in _KEYED:
            with self.subTest(engine=engine):
                self.host.requests.clear()
                self.assertTrue(module._authentication_check(*args))
                self.assertEqual(module._get_available_text_models(*args), ["m-b", "m-a"])
                base = args[1] if len(args) > 1 else None
                self.assertEqual(self._params(), [
                    ("llm.auth_check", {"engine": engine, "api_key": args[0], "base_url": base}),
                    ("llm.models", {"engine": engine, "api_key": args[0], "base_url": base}),
                ])

    def test_openrouter_and_compatible_pass_their_base_url(self) -> None:
        self.assertTrue(translation_openrouter._authentication_check("or-1"))
        translation_openrouter._get_available_text_models("or-1", "https://openrouter.ai/api/v1")
        translation_openai_compatible._get_available_text_models("c-1", "http://proxy/v1")
        self.assertEqual(self._params(), [
            ("llm.auth_check", {"engine": "OpenRouter_API", "api_key": "or-1", "base_url": None}),
            ("llm.models", {"engine": "OpenRouter_API", "api_key": "or-1", "base_url": "https://openrouter.ai/api/v1"}),
            ("llm.models", {"engine": "OpenAI_Compatible", "api_key": "c-1", "base_url": "http://proxy/v1"}),
        ])

    def test_the_compatible_engine_checks_its_key_through_the_openai_function(self) -> None:
        client = translation_openai_compatible.OpenAICompatibleClient(base_url="http://proxy/v1")
        self.assertTrue(client.setAuthKey("c-1"))
        self.assertEqual(client.api_key, "c-1")
        self.assertEqual(self._params(), [
            ("llm.auth_check", {"engine": "OpenAI_API", "api_key": "c-1", "base_url": "http://proxy/v1"}),
        ])

    def test_local_servers_ask_the_host_without_a_key(self) -> None:
        self.assertTrue(translation_lmstudio._authentication_check("http://lm:1234/v1"))
        self.assertEqual(translation_lmstudio._get_available_text_models("http://lm:1234/v1"), ["m-b", "m-a"])
        self.assertTrue(translation_ollama._authentication_check("http://ol:11434"))
        self.assertEqual(translation_ollama._get_available_text_models("http://ol:11434"), ["m-b", "m-a"])
        self.assertEqual(self._params(), [
            ("llm.auth_check", {"engine": "LMStudio", "api_key": None, "base_url": "http://lm:1234/v1"}),
            ("llm.models", {"engine": "LMStudio", "api_key": None, "base_url": "http://lm:1234/v1"}),
            ("llm.auth_check", {"engine": "Ollama", "api_key": None, "base_url": "http://ol:11434"}),
            ("llm.models", {"engine": "Ollama", "api_key": None, "base_url": "http://ol:11434"}),
        ])

    def test_a_failed_check_is_false_where_python_swallowed_errors(self) -> None:
        self.replies["llm.auth_check"] = RuntimeError("HTTP 401: bad key")
        for module, _engine, args in _KEYED:
            self.assertIs(module._authentication_check(*args), False)
        self.assertIs(translation_lmstudio._authentication_check("http://lm"), False)
        self.assertIs(translation_ollama._authentication_check("http://ol"), False)

    def test_a_failed_check_still_escapes_where_python_let_it(self) -> None:
        self.replies["llm.auth_check"] = RuntimeError("request failed")
        with self.assertRaises(utils.RustCallError):
            translation_openrouter._authentication_check("or-1")

    def test_a_false_answer_is_false(self) -> None:
        self.replies["llm.auth_check"] = False
        self.assertIs(translation_openrouter._authentication_check("or-1"), False)
        self.assertIs(translation_ollama._authentication_check("http://ol"), False)

    def test_model_list_errors_follow_each_engines_old_behaviour(self) -> None:
        self.replies["llm.models"] = RuntimeError("HTTP 401")
        # SDK errors used to escape ...
        for module, _engine, args in _KEYED:
            with self.assertRaises(utils.RustCallError):
                module._get_available_text_models(*args)
        with self.assertRaises(utils.RustCallError):
            translation_openrouter._get_available_text_models("k", "http://x")
        # ... the local servers turned them into an empty list ...
        self.assertEqual(translation_lmstudio._get_available_text_models("http://lm"), [])
        self.assertEqual(translation_ollama._get_available_text_models("http://ol"), [])
        # ... and the compatible client's own getModelList does the same.
        client = translation_openai_compatible.OpenAICompatibleClient(base_url="http://proxy/v1")
        client.api_key = "c-1"
        self.assertEqual(client.getModelList(), [])

    def test_clients_use_the_hosts_list_to_validate_a_model_choice(self) -> None:
        client = translation_openai.OpenAIClient()
        client.api_key = "sk-1"
        self.assertEqual(client.getModelList(), ["m-b", "m-a"])
        self.assertTrue(client.setModel("m-a"))
        self.assertFalse(client.setModel("nope"))

        ollama = translation_ollama.OllamaClient()
        self.host.requests.clear()
        self.assertEqual(ollama.getModelList(), ["m-b", "m-a"])
        self.assertEqual([m for m, _ in self._params()], ["llm.auth_check", "llm.models"])  # greeting first

    def test_a_keyless_client_never_calls_the_host(self) -> None:
        for client in (translation_openai.OpenAIClient(), translation_groq.GroqClient(),
                       translation_plamo.PlamoClient(), translation_openrouter.OpenRouterClient()):
            self.assertEqual(client.getModelList(), [])
        self.assertEqual(self.host.requests, [])

    def test_no_request_reaches_the_process_log(self) -> None:
        with patch.object(utils, "printLog") as print_log, patch.object(utils, "printResponse") as print_response:
            translation_openai._authentication_check("sk-1", None)
            translation_openai._get_available_text_models("sk-1", None)
        print_log.assert_not_called()
        print_response.assert_not_called()


class HostNotTakingOverTests(unittest.TestCase):
    """Without the methods in VRCT_RUST_RPC, the SDKs and requests are used as before."""

    def setUp(self) -> None:
        self.host = _Host()
        patcher = patch.object(utils, "_enqueueResponseLine", self.host)
        patcher.start()
        self.addCleanup(patcher.stop)

    def test_the_sdk_and_requests_are_still_used(self) -> None:
        for value in (None, "", "translate.llm", "llm.models"):  # llm.models alone does not switch the check
            with self.subTest(value=value), patch.dict(os.environ, clear=False):
                os.environ.pop(utils._RPC_ENV, None)
                if value is not None:
                    os.environ[utils._RPC_ENV] = value
                sdk = MagicMock()
                with patch.object(translation_openai, "OpenAI", return_value=sdk) as factory:
                    self.assertTrue(translation_openai._authentication_check("sk-1", None))
                factory.assert_called_once()
                sdk.models.list.assert_called_once()
                with patch.object(translation_ollama.requests, "get", return_value=MagicMock(status_code=200)) as get:
                    self.assertTrue(translation_ollama._authentication_check("http://ol"))
                get.assert_called_once()
        self.assertEqual(self.host.lines, [])

    def test_each_call_is_switched_on_by_itself(self) -> None:
        self.host.answer = lambda request: {"ok": True, "result": True}
        sdk = MagicMock()
        sdk.models.list.return_value = MagicMock(data=[MagicMock(id="gpt-4o", root="gpt-4o")])
        with patch.dict(os.environ, {utils._RPC_ENV: "llm.auth_check"}), \
             patch.object(translation_openai, "OpenAI", return_value=sdk):
            self.assertTrue(translation_openai._authentication_check("sk-1", None))
            self.assertEqual(translation_openai._get_available_text_models("sk-1", None), ["gpt-4o"])
        self.assertEqual([r["method"] for r in self.host.requests], ["llm.auth_check"])


if __name__ == "__main__":
    unittest.main()
