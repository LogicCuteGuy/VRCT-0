//! The clipboard sink against the real Windows desktop.
//!
//! Everything that touches the real clipboard or sends keystrokes is
//! `#[ignore]`d so a plain `cargo test` never overwrites the developer's
//! clipboard or types into a window. Run them on purpose, from a desktop
//! session, with:
//!
//!     cargo test -p vrct-core --test clipboard_sink -- --ignored --test-threads=1
//!
//! They read the clipboard through PowerShell and the pasted text out of a
//! separate window process, so a pass means another program sees the text.

use std::sync::Arc;

use serde_json::{json, Value};
use vrct_core::config::ConfigReplica;
use vrct_core::protocol::{parse_sidecar_line, Response};
use vrct_core::sinks::Sinks;

fn line(endpoint: &str, result: Value) -> Response {
    let text = json!({"status": 200, "endpoint": endpoint, "result": result}).to_string();
    parse_sidecar_line(&text).expect("sidecar line parses")
}

fn sinks() -> Sinks {
    Sinks::new(Arc::new(ConfigReplica::default()))
}

#[test]
fn malformed_lines_are_consumed_without_effect() {
    // Deliberately no well-formed line here: that would copy for real.
    let sinks = sinks();
    assert!(sinks.ingest(&line("/internal/clipboard/copy_paste", json!({}))));
    assert!(sinks.ingest(&line("/internal/clipboard/copy_paste", json!({"text": 5}))));
}

#[cfg(windows)]
mod desktop {
    use super::*;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    fn powershell(script: &str) -> Option<String> {
        let preamble = "$ErrorActionPreference='Stop';[Console]::OutputEncoding=[Text.Encoding]::UTF8;";
        let output = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &format!("{preamble}{script}")])
            .stderr(Stdio::null())
            .output()
            .expect("run powershell");
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn b64(text: &str) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(text)
    }

    fn unb64(text: &str) -> String {
        use base64::Engine;
        String::from_utf8(base64::engine::general_purpose::STANDARD.decode(text).unwrap()).unwrap()
    }

    fn read_clipboard() -> Option<String> {
        powershell("[Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes((Get-Clipboard -Raw)))")
            .filter(|text| !text.is_empty())
            .map(|text| unb64(&text))
    }

    /// Puts the developer's text clipboard back afterwards (text only).
    struct RestoreClipboard(Option<String>);

    impl RestoreClipboard {
        fn save() -> Self {
            Self(read_clipboard())
        }
    }

    impl Drop for RestoreClipboard {
        fn drop(&mut self) {
            let text = self.0.clone().unwrap_or_default();
            let decode = "[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String";
            powershell(&format!("Set-Clipboard -Value ({decode}('{}')))", b64(&text)));
        }
    }

    fn unique(label: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("vrct-{label}-{nanos} こんにちは 🙂\nsecond line")
    }

    fn wait_for<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(value) = probe() {
                return value;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    #[test]
    #[ignore = "overwrites the system clipboard"]
    fn the_text_lands_on_the_clipboard_for_other_programs() {
        let _restore = RestoreClipboard::save();
        let text = unique("copy");
        let sinks = sinks();
        assert!(sinks.ingest(&line("/internal/clipboard/copy_paste", json!({"text": text, "window": null}))));
        let seen = wait_for("the text to reach the clipboard", || read_clipboard().filter(|t| *t == text));
        assert_eq!(seen, text);
    }

    /// A throwaway window with a uniquely titled text box that mirrors its
    /// text into a file, so the test needs no other program's cooperation
    /// (and leaves nothing behind, unlike Notepad's restored tabs).
    const WINDOW_SCRIPT: &str = r#"
        Add-Type -AssemblyName System.Windows.Forms
        $mirror = $args[1]
        $form = New-Object Windows.Forms.Form
        $form.Text = $args[0]
        $box = New-Object Windows.Forms.TextBox
        $box.Multiline = $true
        $box.Dock = 'Fill'
        $form.Controls.Add($box)
        $form.Add_Shown({ $form.Activate(); $box.Focus() })
        $timer = New-Object Windows.Forms.Timer
        $timer.Interval = 200
        $timer.Add_Tick({ [IO.File]::WriteAllText($mirror, $box.Text) })
        $timer.Start()
        [Windows.Forms.Application]::Run($form)
    "#;

    #[test]
    #[ignore = "overwrites the clipboard, takes the focus and types Ctrl+V"]
    fn a_named_window_is_focused_and_receives_the_paste() {
        let _restore = RestoreClipboard::save();
        let title: String = unique("window").chars().filter(char::is_ascii_alphanumeric).collect();
        let dir = std::env::temp_dir();
        let script = dir.join(format!("{title}.ps1"));
        let mirror = dir.join(format!("{title}.txt"));
        std::fs::write(&script, WINDOW_SCRIPT).unwrap();

        let mut window = Command::new("powershell")
            .args(["-NoProfile", "-STA", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(&script)
            .args([title.as_str(), mirror.to_str().unwrap()])
            .stderr(Stdio::null())
            .spawn()
            .expect("start the test window");
        let result = std::panic::catch_unwind(|| {
            wait_for("the test window to appear", || mirror.exists().then_some(()));
            std::thread::sleep(Duration::from_secs(1));

            let text = unique("paste");
            let sinks = sinks();
            // The title is unique to this window, so nothing else can be hit.
            sinks.ingest(&line(
                "/internal/clipboard/copy_paste",
                json!({"text": text, "window": title}),
            ));
            let pasted = wait_for("the text to appear in the window", || {
                std::fs::read_to_string(&mirror)
                    .ok()
                    .filter(|doc| doc.contains("vrct-paste-"))
            });
            // A Windows text box stores line breaks as CRLF.
            assert_eq!(pasted.replace("\r\n", "\n"), text);
        });
        let _ = window.kill();
        let _ = window.wait();
        let _ = std::fs::remove_file(&script);
        let _ = std::fs::remove_file(&mirror);
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }
}
