import { useStore_AvailableReleases } from "@store";
import { useBackendRequest } from "@useBackendRequest";

export const useAvailableReleases = () => {
    const { sendBackendRequest } = useBackendRequest();
    const { currentAvailableReleases, updateAvailableReleases, pendingAvailableReleases } = useStore_AvailableReleases();

    const getAvailableReleases = () => {
        pendingAvailableReleases();
        sendBackendRequest("/get/data/available_releases");
    };

    const updateAvailableReleasesFromBackend = (payload) => {
        updateAvailableReleases(Array.isArray(payload) ? payload : []);
    };

    return {
        currentAvailableReleases,
        getAvailableReleases,
        updateAvailableReleasesFromBackend,
    };
};
