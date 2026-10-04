import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const [program, ...args] = process.argv.slice(2);
if (!program) throw new Error("native_env requires a command");
const result = process.platform === "win32"
    ? spawnSync("powershell.exe", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File",
        fileURLToPath(new URL("./windows_native.ps1", import.meta.url)), program, ...args], { stdio: "inherit" })
    : spawnSync(program, args, { stdio: "inherit" });
if (result.error) console.error(result.error.message);
process.exit(result.status ?? 1);
