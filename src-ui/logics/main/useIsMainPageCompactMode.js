import { useStore_IsMainPageCompactMode } from "@store";
import { useBackendRequest } from "@useBackendRequest";

export const useIsMainPageCompactMode = () => {
    const { sendBackendRequest } = useBackendRequest();
    const { currentIsMainPageCompactMode, updateIsMainPageCompactMode } = useStore_IsMainPageCompactMode();

    const getIsMainPageCompactMode = () => {
        sendBackendRequest("/get/data/main_window_sidebar_compact_mode");
    };

    const toggleIsMainPageCompactMode = () => {
        if (currentIsMainPageCompactMode.data) {
            sendBackendRequest("/set/disable/main_window_sidebar_compact_mode");
        } else {
            sendBackendRequest("/set/enable/main_window_sidebar_compact_mode");
        }
    };

    return {
        currentIsMainPageCompactMode,
        getIsMainPageCompactMode,
        toggleIsMainPageCompactMode,
        updateIsMainPageCompactMode,
    };
};