"""DeepL の Rust 委譲 (RPC bridge) に関するテスト。

ホストが VRCT_RUST_RPC に "translate.deepl" / "translate.deepl.check" を載せて起動した
場合だけ、鍵の確認と翻訳の HTTP 呼び出しをホストに任せる。載っていなければ従来通り
`deepl` SDK を使う。
"""

import os
import unittest
from unittest.mock import MagicMock, patch

import utils
from models.translation.translation_languages import loadTranslationLanguages
from models.translation.translation_translator import Translator
from test_translate_llm_bridge import _Host

loadTranslationLanguages(path=os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

_BOTH = "translate.deepl,translate.deepl.check"


class TranslateDeepLBridgeTests(unittest.TestCase):
    def setUp(self) -> None:
        self.translator = Translator()
        self.translator.is_enable_translators = True
        self.host = _Host(answer=lambda request: {
            "ok": True,
            "result": True if request["method"].endswith(".check") else "rust:" + request["params"]["text"],
        })
        patcher = patch.object(utils, "_enqueueResponseLine", self.host)
        patcher.start()
        self.addCleanup(patcher.stop)

    def _translate(self, **overrides):
        args = dict(
            translator_name="DeepL_API",
            weight_type="",
            source_language="Japanese",
            target_language="English",
            target_country="United States",
            message="こんにちは",
        )
        args.update(overrides)
        return self.translator.translate(**args)

    def _methods(self):
        return [request["method"] for request in self.host.requests]

    def test_the_sdk_is_used_when_the_host_has_not_taken_it_over(self) -> None:
        for value in (None, "", "translate.llm"):
            with self.subTest(value=value), patch.dict(os.environ, clear=False):
                os.environ.pop(utils._RPC_ENV, None)
                if value is not None:
                    os.environ[utils._RPC_ENV] = value
                sdk = MagicMock()
                sdk.translate_text.return_value.text = "sdk:hello"
                with patch("models.translation.translation_translator.DeepLClient", return_value=sdk) as factory:
                    self.assertTrue(self.translator.authenticationDeepLAuthKey("key:fx"))
                    self.assertEqual(self._translate(), "sdk:hello")
                factory.assert_called_once_with("key:fx")
                self.assertEqual(sdk.translate_text.call_count, 2)  # key check + translation
        self.assertEqual(self.host.lines, [])

    def test_the_key_check_and_translation_both_run_in_the_host(self) -> None:
        with patch.dict(os.environ, {utils._RPC_ENV: _BOTH}), \
             patch("models.translation.translation_translator.DeepLClient") as sdk:
            self.assertTrue(self.translator.authenticationDeepLAuthKey("secret:fx"))
            self.assertEqual(self._translate(), "rust:こんにちは")
        sdk.assert_not_called()

        check, translation = self.host.requests
        self.assertEqual(check["method"], "translate.deepl.check")
        self.assertEqual(check["params"], {"auth_key": "secret:fx"})
        self.assertEqual(translation["method"], "translate.deepl")
        source, target = Translator.getLanguageCode("DeepL_API", "", "United States", "Japanese", "English")
        self.assertEqual(translation["params"], {
            "auth_key": "secret:fx",
            "text": "こんにちは",
            "source_lang": source,
            "target_lang": target,
        })

    def test_regional_target_codes_still_come_from_python(self) -> None:
        with patch.dict(os.environ, {utils._RPC_ENV: _BOTH}):
            self.translator.deepl_client = None
            self.translator.authenticationDeepLAuthKey("k")
            self.host.requests.clear()
            self._translate(target_country="United Kingdom")
            british = self.host.requests[0]["params"]["target_lang"]
            self._translate(target_country="Canada")
            american = self.host.requests[1]["params"]["target_lang"]
        self.assertNotEqual(british, american)

    def test_a_rejected_key_leaves_the_engine_unauthenticated(self) -> None:
        self.host.answer = lambda request: {"ok": False, "error": "HTTP 403: Forbidden"}
        with patch.dict(os.environ, {utils._RPC_ENV: _BOTH}), patch("models.translation.translation_translator.errorLogging"):
            self.assertFalse(self.translator.authenticationDeepLAuthKey("bad"))
        self.assertIsNone(self.translator.deepl_client)
        self.assertEqual(self._methods(), ["translate.deepl.check"])

    def test_an_empty_key_never_reaches_the_host(self) -> None:
        with patch.dict(os.environ, {utils._RPC_ENV: _BOTH}), patch("models.translation.translation_translator.errorLogging"):
            self.assertFalse(self.translator.authenticationDeepLAuthKey(""))
        self.assertIsNone(self.translator.deepl_client)
        self.assertEqual(self.host.requests, [])

    def test_a_host_failure_is_a_failed_translation_not_a_crash(self) -> None:
        with patch.dict(os.environ, {utils._RPC_ENV: _BOTH}), patch("models.translation.translation_translator.errorLogging"):
            self.translator.authenticationDeepLAuthKey("k")
            self.host.answer = lambda request: {"ok": False, "error": "HTTP 456: Quota Exceeded"}
            self.assertIs(self._translate(), False)

    def test_without_an_authenticated_key_translation_fails_before_the_host(self) -> None:
        with patch.dict(os.environ, {utils._RPC_ENV: _BOTH}):
            self.assertIs(self._translate(), False)
        self.assertEqual(self.host.requests, [])

    def test_unsupported_language_never_reaches_the_host(self) -> None:
        with patch.dict(os.environ, {utils._RPC_ENV: _BOTH}):
            self.translator.authenticationDeepLAuthKey("k")
            self.host.requests.clear()
            self.assertIsNone(self._translate(source_language="Klingon"))
        self.assertEqual(self.host.requests, [])

    def test_each_call_is_switched_on_by_itself(self) -> None:
        # Host does the key check only: translation still goes through the SDK.
        sdk = MagicMock()
        sdk.translate_text.return_value.text = "sdk:hello"
        self.host.answer = lambda request: {"ok": True, "result": True}
        with patch.dict(os.environ, {utils._RPC_ENV: "translate.deepl.check"}), \
             patch("models.translation.translation_translator.DeepLClient", return_value=sdk):
            self.assertTrue(self.translator.authenticationDeepLAuthKey("k"))
            self.assertEqual(self._translate(), "sdk:hello")
        self.assertEqual(self._methods(), ["translate.deepl.check"])
        sdk.translate_text.assert_called_once()  # the translation only; the check went to the host

        # Host does the translation only: the key is checked by a blank translation through it.
        self.host.requests.clear()
        self.host.answer = lambda request: {"ok": True, "result": "rust:" + request["params"]["text"]}
        self.translator.deepl_client = None
        with patch.dict(os.environ, {utils._RPC_ENV: "translate.deepl"}):
            self.assertTrue(self.translator.authenticationDeepLAuthKey("k"))
        self.assertEqual(self._methods(), ["translate.deepl"])
        self.assertEqual(self.host.requests[0]["params"]["text"], " ")
        self.assertEqual(self.host.requests[0]["params"]["target_lang"], "EN-US")


if __name__ == "__main__":
    unittest.main()
