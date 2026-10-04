import blackLogo from "@images/vrct-0-logo-black.png";
import whiteLogo from "@images/vrct-0-logo-white.png";
import styles from "./BrandLogo.module.scss";

export const BrandLogo = ({ className = "" }) => <>
    <img src={blackLogo} className={`${className} ${styles.light}`} alt="VRCT-0 — VRChat Chatbox Translator & Transcription" />
    <img src={whiteLogo} className={`${className} ${styles.dark}`} alt="VRCT-0 — VRChat Chatbox Translator & Transcription" />
</>;
