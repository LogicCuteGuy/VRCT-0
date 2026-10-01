import { invoke } from "@tauri-apps/api/core";
import { encode } from "js-base64";

export const useStdoutToPython = () => {
    const asyncStdoutToPython = async (path, value = undefined) => {
        const data = value !== undefined ? encode(JSON.stringify(value)) : null;

        // The Rust backend answers asynchronously through the "backend-response" event.
        await invoke("backend_request", { endpoint: path, data }).catch((err) => {
            console.log(err);
        });
    };
    return { asyncStdoutToPython };
};
