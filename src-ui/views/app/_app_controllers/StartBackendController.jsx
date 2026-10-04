import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useEffect } from "react";
import { useBackendRequest } from "@useBackendRequest";
import { useReceiveRoutes } from "@useReceiveRoutes";
import { useStore_SelectableFontFamilyList } from "@store";
import { arrayToObject } from "@utils";
import { useNotificationStatus } from "@logics_common";

export const StartBackendController = () => {
    const { receiveRoutes } = useReceiveRoutes();
    const { sendBackendRequest: request } = useBackendRequest();
    const { updateSelectableFontFamilyList } = useStore_SelectableFontFamilyList();
    const { showNotification_Error } = useNotificationStatus();

    useEffect(() => {
        let active = true;
        let watchdog = null;
        const unlisten = [];
        const subscribe = async (name, handler) => {
            const stop = await listen(name, handler);
            if (active) unlisten.push(stop);
            else stop();
        };
        const start = async () => {
            try {
                await Promise.all([
                    subscribe("backend-response", ({ payload }) => {
                        try { receiveRoutes(payload); }
                        catch (error) { console.error("Backend response", error, payload); }
                    }),
                    subscribe("backend-error", ({ payload }) => {
                        showNotification_Error(String(payload), { hide_duration: null });
                    }),
                ]);
                if (!active) return;
                await invoke("backend_start");
                if (!active) return;
                watchdog = setInterval(() => request("/run/feed_watchdog"), 20000);
                const fonts = await invoke("get_font_list");
                if (active) updateSelectableFontFamilyList(arrayToObject(
                    fonts.sort((a, b) => a.localeCompare(b, undefined, { sensitivity: "base" }))
                ));
            } catch (error) {
                if (active) showNotification_Error(String(error), { hide_duration: null });
            }
        };
        start();
        return () => {
            active = false;
            if (watchdog !== null) clearInterval(watchdog);
            unlisten.forEach(stop => stop());
        };
    }, []);
    return null;
};
