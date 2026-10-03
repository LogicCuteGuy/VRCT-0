import { useStore_IsVrctAvailable } from "@store";
import { useNotificationStatus } from "@logics_common";
import { HomepageLinkButton } from "@common_components";

export const useIsVrctAvailable = () => {
    const { currentIsVrctAvailable, updateIsVrctAvailable } = useStore_IsVrctAvailable();
    const { showNotification_Success, showNotification_Error } = useNotificationStatus();

    const handleAiModelsAvailability = (is_ai_models_available) => {
        if (is_ai_models_available === false) {
            const ErrorComponent = () => {
                return (
                    <div>
                        <p>Local AI models are unavailable. Cloud engines remain available; download a local model in Settings to use CTranslate2 or Whisper.</p>
                        <p>If this error occurs frequently, try the following:</p>
                        <HomepageLinkButton
                            homepage_link="https://github.com/misyaguziya/VRCT/wiki/Manual-Installation-of-AI-Model-Weights"
                        />
                    </div>
                );
            };
            showNotification_Error(ErrorComponent, { hide_duration: null });
        }
    };

    return {
        currentIsVrctAvailable,
        updateIsVrctAvailable,

        handleAiModelsAvailability,
    };
};
