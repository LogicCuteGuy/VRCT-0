import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useRef } from "react";

import { useStdoutToPython } from "@useStdoutToPython";
import { useReceiveRoutes } from "@useReceiveRoutes";
import { useStore_SelectableFontFamilyList } from "@store";
import { arrayToObject } from "@utils";

import {
    useNotificationStatus,
} from "@logics_common";

export const StartPythonController = () => {
    const { asyncStartPython } = useStartPython();
    const hasRunRef = useRef(false);
    const watchdogIntervalIdRef = useRef(null);
    const { asyncFetchFonts } = useAsyncFetchFonts();
    const { asyncStdoutToPython } = useStdoutToPython();

    useEffect(() => {
        if (hasRunRef.current) {
            return () => {
                if (watchdogIntervalIdRef.current !== null) {
                    clearInterval(watchdogIntervalIdRef.current);
                    watchdogIntervalIdRef.current = null;
                }
            };
        }

        // StrictMode で effect が再実行されても sidecar 起動を二重化しない。
        // 非同期処理の完了前に cleanup が走るため、interval ID は effect の
        // ローカル変数ではなく ref で保持する。
        hasRunRef.current = true;
        const startPython = async () => {
            try {
                await asyncStartPython();
                if (watchdogIntervalIdRef.current === null) {
                    watchdogIntervalIdRef.current = startFeedingToWatchDogController(asyncStdoutToPython);
                }
                asyncFetchFonts();
            } catch (err) {
                console.error(err);
            }
        };
        startPython();

        return () => {
            if (watchdogIntervalIdRef.current !== null) {
                clearInterval(watchdogIntervalIdRef.current);
                watchdogIntervalIdRef.current = null;
            }
        };
    }, []);

    return null;
};

const useStartPython = () => {
    const { receiveRoutes } = useReceiveRoutes();
    const { showNotification_Success, showNotification_Error } = useNotificationStatus();

    const asyncStartPython = async () => {
        // Responses (already parsed by the Rust backend) arrive as events.
        // Subscribe before starting the backend so no early response is lost.
        await listen("backend-response", (event) => {
            try {
                receiveRoutes(event.payload);
            } catch (error) {
                console.log(error, event.payload);
            }
        });
        await listen("backend-stderr", (event) => {
            const line = event.payload;
            // Python の warnings.warn() は既定で stderr に書き出される。良性の警告
            // (FutureWarning 等: 依存ライブラリの将来非互換の予告など) まで致命的な
            // エラー通知に昇格させると、実際にはクラッシュしていないのに
            // 「An error occurred」ダイアログが出てしまう。警告行はログに残すだけにする。
            if (typeof line === "string" && /\b[A-Za-z]*Warning: /.test(line)) {
                console.warn("stderr (warning, ignored)", line);
                return;
            }
            showNotification_Error(
                `An error occurred. Please restart VRCT or contact the developers. The last line:${JSON.stringify(line)}`, { hide_duration: null }
            );
            console.error("stderr", line);
        });
        await invoke("backend_start");
    };

    return { asyncStartPython };
};

const useAsyncFetchFonts = () => {
    const { updateSelectableFontFamilyList } = useStore_SelectableFontFamilyList();
    const asyncFetchFonts = async () => {
        try {
            let fonts = await invoke("get_font_list");
            fonts = fonts.sort((a, b) => a.localeCompare(b, undefined, { sensitivity: "base" }));
            updateSelectableFontFamilyList(arrayToObject(fonts));
        } catch (error) {
            console.error("Error fetching fonts:", error);
        }
    };
    return { asyncFetchFonts };
};

const startFeedingToWatchDogController = (asyncStdoutToPython) => {
    return setInterval(() => {
        asyncStdoutToPython("/run/feed_watchdog");
    }, 20000); // 20000ミリ秒 = 20秒
};
