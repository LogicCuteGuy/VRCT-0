import { useBackendRequest } from "@useBackendRequest";

export const useUpdateSoftware = () => {
    const { sendBackendRequest } = useBackendRequest();
    const updateSoftware = (target_version) => {
        sendBackendRequest("/run/update_software", target_version);
    };

    const updateSoftware_CUDA = (target_version) => {
        sendBackendRequest("/run/update_cuda_software", target_version);
    };

    return {
        updateSoftware,
        updateSoftware_CUDA,
    };
};