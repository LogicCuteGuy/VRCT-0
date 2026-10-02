"""LLM 翻訳の Rust 委譲 (RPC bridge) に関するテスト。

Rust 側が VRCT_RUST_RPC に "translate.llm" を載せて起動した場合だけ、LLM 系エンジンの
翻訳は `/internal/rpc/request` を stdout に出して Rust の答えを待つ。載っていない場合
(単体起動・未移植) は従来通り各クライアントの translate() を呼ぶ。
"""

import json
import os
import threading
import time
import unittest
from unittest.mock import patch

import utils
from mainloop import Main
from models.translation.translation_languages import loadTranslationLanguages
from models.translation.translation_translator import Translator

loadTranslationLanguages(path=os.path.dirname(os.path.dirname(os.path.abspath(__file__))))


class _FakeClient:
    """認証済みの LLM クライアントの、翻訳に関わる状態だけを持つ代役。"""

    def __init__(self, api_key="sk-abc", model="m-1", base_url=None, history=None):
        self.api_key = api_key
        self.model = model
        self.base_url = base_url
        self._context_history = history or []
        self.calls = []

    def setContextHistory(self, items):
        self._context_history = items or []

    def translate(self, text, input_lang, output_lang):
        self.calls.append((text, input_lang, output_lang))
        return f"python:{text}"


class _Host:
    """Rust ホストの代役: 出力された RPC 要求を記録し、台本の答えを返す。"""

    def __init__(self, answer=None):
        self.requests = []
        self.answer = answer or (lambda request: {"ok": True, "result": "rust:" + request["params"]["text"]})
        self.lines = []

    def __call__(self, line):
        message = json.loads(line)
        self.lines.append(message)
        if message["endpoint"] != utils.RUST_RPC_REQUEST_ENDPOINT:
            return
        request = message["result"]
        self.requests.append(request)
        answer = self.answer(request)
        if answer is not None:
            utils.resolveRustCall({"id": request["id"], **answer})


class TranslateLlmBridgeTests(unittest.TestCase):
    def setUp(self) -> None:
        self.translator = Translator()
        self.host = _Host()
        patcher = patch.object(utils, "_enqueueResponseLine", self.host)
        patcher.start()
        self.addCleanup(patcher.stop)

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

    def test_python_translates_itself_when_the_host_has_not_taken_it_over(self) -> None:
        for value in (None, "", "translate.other"):
            with self.subTest(value=value), patch.dict(os.environ, clear=False):
                os.environ.pop(utils._RPC_ENV, None)
                if value is not None:
                    os.environ[utils._RPC_ENV] = value
                client = _FakeClient()
                self._use_client("OpenAI_API", client)
                self.assertEqual(self._translate("OpenAI_API"), "python:こんにちは")
                self.assertEqual(len(client.calls), 1)
        self.assertEqual(self.host.lines, [])

    def test_every_llm_engine_is_delegated_with_its_own_state(self) -> None:
        engines = ["OpenAI_API", "Gemini_API", "Groq_API", "OpenRouter_API", "Plamo_API",
                   "OpenAI_Compatible", "LMStudio", "Ollama"]
        for engine in engines:
            with self.subTest(engine=engine), patch.dict(os.environ, {utils._RPC_ENV: "translate.llm"}):
                client = _FakeClient(api_key="sk-" + engine, model="model-" + engine, base_url="http://x/" + engine)
                self._use_client(engine, client)
                self.host.requests.clear()
                try:
                    result = self._translate(engine)
                except Exception as error:  # pragma: no cover - names the engine
                    self.fail(f"{engine}: {error}")
                self.assertEqual(result, "rust:こんにちは", engine)
                self.assertEqual(client.calls, [], f"{engine} must not run the Python client")
                (request,) = self.host.requests
                self.assertEqual(request["method"], "translate.llm")
                params = request["params"]
                self.assertEqual(params["engine"], engine)
                self.assertEqual(params["api_key"], "sk-" + engine)
                self.assertEqual(params["model"], "model-" + engine)
                self.assertEqual(params["base_url"], "http://x/" + engine)
                self.assertEqual(params["text"], "こんにちは")
                # Language *codes* as resolved by getLanguageCode, not display names.
                self.assertEqual((params["input_lang"], params["output_lang"]), self._codes(engine))

    def _codes(self, engine):
        return Translator.getLanguageCode(engine, "", "United States", "Japanese", "English")

    def test_the_latest_conversation_history_is_sent_and_kept_between_calls(self) -> None:
        history = [{"source": "chat", "text": "hi", "timestamp": "2026-10-02T10:00:00"}]
        client = _FakeClient()
        self._use_client("OpenAI_API", client)
        with patch.dict(os.environ, {utils._RPC_ENV: "translate.llm"}):
            self._translate("OpenAI_API", context_history=history)
            self._translate("OpenAI_API")  # empty history must not reset it (existing behaviour)
        self.assertEqual([r["params"]["history"] for r in self.host.requests], [history, history])

    def test_a_host_failure_is_a_failed_translation_not_a_crash(self) -> None:
        self.host.answer = lambda request: {"ok": False, "error": "HTTP 401: bad key"}
        self._use_client("OpenAI_API", _FakeClient())
        with patch.dict(os.environ, {utils._RPC_ENV: "translate.llm"}), patch("models.translation.translation_translator.errorLogging"):
            self.assertIs(self._translate("OpenAI_API"), False)

    def test_an_unconfigured_engine_still_fails_without_calling_the_host(self) -> None:
        with patch.dict(os.environ, {utils._RPC_ENV: "translate.llm"}):
            self.assertIs(self._translate("OpenAI_API"), False)
            self.assertIs(self._translate("Ollama"), False)
        self.assertEqual(self.host.requests, [])

    def test_unsupported_language_never_reaches_the_host(self) -> None:
        self._use_client("Plamo_API", _FakeClient())
        with patch.dict(os.environ, {utils._RPC_ENV: "translate.llm"}):
            self.assertIsNone(self._translate("Plamo_API", source_language="Klingon"))
        self.assertEqual(self.host.requests, [])

    def test_same_language_is_returned_untouched(self) -> None:
        self._use_client("OpenAI_API", _FakeClient())
        with patch.dict(os.environ, {utils._RPC_ENV: "translate.llm"}):
            self.assertEqual(self._translate("OpenAI_API", target_language="Japanese"), "こんにちは")
        self.assertEqual(self.host.requests, [])

    def test_the_request_line_skips_the_process_log(self) -> None:
        self._use_client("OpenAI_API", _FakeClient())
        with patch.dict(os.environ, {utils._RPC_ENV: "translate.llm"}), \
             patch.object(utils, "printLog") as print_log, patch.object(utils, "printResponse") as print_response:
            self._translate("OpenAI_API")
        print_log.assert_not_called()
        print_response.assert_not_called()


class CallRustTests(unittest.TestCase):
    def setUp(self) -> None:
        self.host = _Host()
        patcher = patch.object(utils, "_enqueueResponseLine", self.host)
        patcher.start()
        self.addCleanup(patcher.stop)

    def test_returns_the_result_of_the_matching_call(self) -> None:
        self.assertEqual(utils.callRust("m", {"text": "a"}), "rust:a")

    def test_timeout_raises_and_forgets_the_call_so_a_late_answer_is_ignored(self) -> None:
        self.host.answer = lambda request: None
        with self.assertRaises(utils.RustCallError):
            utils.callRust("m", {"text": "a"}, timeout=0.05)
        self.assertEqual(utils._rust_calls_pending, {})
        self.assertFalse(utils.resolveRustCall({"id": self.host.requests[0]["id"], "ok": True, "result": "late"}))

    def test_failure_answers_raise_with_the_reason(self) -> None:
        self.host.answer = lambda request: {"ok": False, "error": "nope"}
        with self.assertRaisesRegex(utils.RustCallError, "nope"):
            utils.callRust("m", {})

    def test_threads_waiting_side_by_side_get_their_own_answers(self) -> None:
        pending = {}

        def answer(request):
            pending[request["params"]["text"]] = request["id"]
            return None  # answered by hand below, out of order

        self.host.answer = answer
        results = {}

        def worker(text):
            results[text] = utils.callRust("m", {"text": text}, timeout=5)

        threads = [threading.Thread(target=worker, args=(t,)) for t in ("one", "two", "three")]
        for thread in threads:
            thread.start()
        deadline = time.time() + 5
        while len(pending) < 3 and time.time() < deadline:
            time.sleep(0.01)
        for text in ("three", "one", "two"):
            self.assertTrue(utils.resolveRustCall({"id": pending[text], "ok": True, "result": text.upper()}))
        for thread in threads:
            thread.join(5)
        self.assertEqual(results, {"one": "ONE", "two": "TWO", "three": "THREE"})

    def test_junk_answers_are_ignored(self) -> None:
        for junk in (None, [], "x", {}, {"id": "nope"}, {"id": 10 ** 9}):
            self.assertFalse(utils.resolveRustCall(junk))


class MainloopRoutingTests(unittest.TestCase):
    def test_a_host_answer_resolves_the_call_and_is_never_queued_or_logged(self) -> None:
        import base64
        from unittest.mock import MagicMock

        main = Main(controller_instance=MagicMock(), mapping_data={})
        host = _Host(answer=lambda request: None)
        result = {}

        with patch.object(utils, "_enqueueResponseLine", host):
            thread = threading.Thread(target=lambda: result.update(value=utils.callRust("m", {"text": "x"}, timeout=5)))
            thread.start()
            deadline = time.time() + 5
            while not host.requests and time.time() < deadline:
                time.sleep(0.01)
            payload = base64.b64encode(json.dumps({"id": host.requests[0]["id"], "ok": True, "result": "done"}).encode()).decode()
            line = json.dumps({"endpoint": utils.RUST_RPC_RESPONSE_ENDPOINT, "data": payload})
            with patch("mainloop.printLog") as print_log:
                main._handleInputLine(line + "\n")
            thread.join(5)

        self.assertEqual(result["value"], "done")
        self.assertTrue(main.queue.empty())
        print_log.assert_not_called()

    def test_ordinary_requests_are_still_queued(self) -> None:
        from unittest.mock import MagicMock

        main = Main(controller_instance=MagicMock(), mapping_data={})
        with patch("mainloop.printLog"):
            main._handleInputLine(json.dumps({"endpoint": "/get/data/version"}) + "\n")
        self.assertEqual(main.queue.get_nowait(), ("/get/data/version", None, 0))


if __name__ == "__main__":
    unittest.main()
