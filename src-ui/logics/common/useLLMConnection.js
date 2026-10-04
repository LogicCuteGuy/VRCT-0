import { useBackendRequest } from "@useBackendRequest";
import {
    useStore_IsLMStudioConnected,
    useStore_IsOllamaConnected,
} from "@store";

export const useLLMConnection = () => {
    const { sendBackendRequest } = useBackendRequest();
    const {
        currentIsLMStudioConnected,
        updateIsLMStudioConnected,
        pendingIsLMStudioConnected,
    } = useStore_IsLMStudioConnected();
    const {
        currentIsOllamaConnected,
        updateIsOllamaConnected,
        pendingIsOllamaConnected,
    } = useStore_IsOllamaConnected();

    const checkConnection_LMStudio = () => {
        pendingIsLMStudioConnected();
        sendBackendRequest("/run/lmstudio_connection");
    };
    const setConnectionStatus_LMStudio = (is_connected) => {
        updateIsLMStudioConnected(is_connected);
    };

    const checkConnection_Ollama = () => {
        pendingIsOllamaConnected();
        sendBackendRequest("/run/ollama_connection");
    };
    const setConnectionStatus_Ollama = (is_connected) => {
        updateIsOllamaConnected(is_connected);
    };

    return {
        currentIsLMStudioConnected,
        updateIsLMStudioConnected,
        setConnectionStatus_LMStudio,
        checkConnection_LMStudio,

        currentIsOllamaConnected,
        updateIsOllamaConnected,
        setConnectionStatus_Ollama,
        checkConnection_Ollama,
    };
};