import { useEffect } from "react";
import { useAppearance } from "@logics_configs";
import { useI18n } from "@useI18n";

export const FontFamilyController = () => {
    const { currentSelectedFontFamily } = useAppearance();
    const { i18n } = useI18n();
    useEffect(() => {
        const selected = currentSelectedFontFamily.data || "Segoe UI";
        document.documentElement.style.setProperty("--font_family", i18n.language === "th"
            ? `"Leelawadee UI", "Noto Sans Thai", "Tahoma", ${selected}, sans-serif`
            : selected);
    }, [currentSelectedFontFamily.data, i18n.language]);

    return null;
};
