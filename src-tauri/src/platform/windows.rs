//! UI Automation focus/password checks plus Unicode SendInput. No clipboard,
//! virtual Enter, focus stealing, or elevation bypass. Requires Windows testing.
use std::{
    mem::size_of,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, HWND},
        Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
                CoUninitialize,
            },
            Threading::{
                OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32,
                PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
            },
        },
        UI::{
            Accessibility::*,
            Input::KeyboardAndMouse::*,
            WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
        },
    },
    core::Interface,
};
fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}
struct Apartment;
impl Apartment {
    fn open() -> Result<Self, String> {
        // SAFETY: the dedicated delivery thread owns and balances this COM apartment.
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED)
                .ok()
                .map_err(error)?;
        }
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}
struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

pub struct Destination {
    automation: IUIAutomation,
    element: IUIAutomationElement,
    window: HWND,
    pid: u32,
    terminal: bool,
    value: Option<IUIAutomationValuePattern>,
    expected: Option<String>,
    selection: Option<IUIAutomationTextRange>,
    failed: bool,
    _apartment: Apartment,
}
fn process(pid: u32) -> Result<String, String> {
    unsafe {
        let process =
            Handle(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).map_err(error)?);
        let mut token = HANDLE::default();
        OpenProcessToken(process.0, TOKEN_QUERY, &mut token).map_err(error)?;
        let token = Handle(token);
        let mut elevation = TOKEN_ELEVATION::default();
        let mut length = 0;
        GetTokenInformation(
            token.0,
            TokenElevation,
            Some((&mut elevation as *mut TOKEN_ELEVATION).cast()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut length,
        )
        .map_err(error)?;
        if elevation.TokenIsElevated != 0 {
            return Err("Elevated applications are not supported. Run both applications without administrator privileges.".into());
        }
        let mut path = [0u16; 32768];
        let mut length = path.len() as u32;
        QueryFullProcessImageNameW(
            process.0,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(path.as_mut_ptr()),
            &mut length,
        )
        .map_err(error)?;
        Ok(String::from_utf16_lossy(&path[..length as usize])
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase())
    }
}
fn selection(element: &IUIAutomationElement) -> Result<Option<IUIAutomationTextRange>, String> {
    unsafe {
        let Ok(pattern) =
            element.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
        else {
            return Ok(None);
        };
        let ranges = pattern.GetSelection().map_err(error)?;
        if ranges.Length().map_err(error)? != 1 {
            return Err("Clear the destination selection before dictating.".into());
        }
        let range = ranges.GetElement(0).map_err(error)?;
        if range
            .CompareEndpoints(
                TextPatternRangeEndpoint_Start,
                &range,
                TextPatternRangeEndpoint_End,
            )
            .map_err(error)?
            != 0
        {
            return Err("Clear the destination selection before dictating.".into());
        }
        Ok(Some(range))
    }
}
impl Destination {
    pub fn capture(allow_terminal: bool) -> Result<Self, String> {
        let apartment = Apartment::open()?;
        unsafe {
            let automation2: IUIAutomation2 =
                CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER).map_err(error)?;
            automation2.SetConnectionTimeout(300).map_err(error)?;
            automation2.SetTransactionTimeout(300).map_err(error)?;
            let automation: IUIAutomation = automation2.cast().map_err(error)?;
            let window = GetForegroundWindow();
            if window.0.is_null() {
                return Err("No foreground application.".into());
            }
            let element = automation.GetFocusedElement().map_err(error)?;
            let pid = element.CurrentProcessId().map_err(error)? as u32;
            if pid == std::process::id() {
                return Err("Click an external text field to bind desktop typing.".into());
            }
            let mut foreground_pid = 0;
            GetWindowThreadProcessId(window, Some(&mut foreground_pid));
            if foreground_pid != pid {
                return Err("Focused field is not in the foreground application.".into());
            }
            if element.CurrentIsPassword().map_err(error)?.as_bool() {
                return Err("Password fields are refused.".into());
            }
            if !element.CurrentIsEnabled().map_err(error)?.as_bool()
                || !element.CurrentHasKeyboardFocus().map_err(error)?.as_bool()
            {
                return Err("Destination is not enabled and focused.".into());
            }
            let executable = process(pid)?;
            let class = element
                .CurrentClassName()
                .map_err(error)?
                .to_string()
                .to_ascii_lowercase();
            let terminal = [
                "windowsterminal.exe",
                "openconsole.exe",
                "conhost.exe",
                "cmd.exe",
                "powershell.exe",
                "pwsh.exe",
                "wezterm-gui.exe",
                "alacritty.exe",
            ]
            .contains(&executable.as_str())
                || class.contains("termcontrol")
                || class.contains("xterm");
            if terminal && !allow_terminal {
                return Err("Enable terminal typing in Settings first.".into());
            }
            let value = element
                .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                .ok();
            let editable = value
                .as_ref()
                .is_some_and(|v| v.CurrentIsReadOnly().is_ok_and(|b| !b.as_bool()));
            let text_edit = element
                .GetCurrentPatternAs::<IUIAutomationTextEditPattern>(UIA_TextEditPatternId)
                .is_ok();
            if !terminal && !editable && !text_edit {
                return Err(
                    "The focused control does not expose a writable UI Automation text field."
                        .into(),
                );
            }
            let expected = if terminal {
                None
            } else {
                value
                    .as_ref()
                    .map(|v| v.CurrentValue().map(|v| v.to_string()).map_err(error))
                    .transpose()?
            };
            let selection = if terminal { None } else { selection(&element)? };
            Ok(Self {
                automation,
                element,
                window,
                pid,
                terminal,
                value,
                expected,
                selection,
                failed: false,
                _apartment: apartment,
            })
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        self.check()
    }

    fn check(&self) -> Result<(), String> {
        unsafe {
            if self.failed || GetForegroundWindow() != self.window {
                return Err("Foreground application changed.".into());
            }
            let focused = self.automation.GetFocusedElement().map_err(error)?;
            if !self
                .automation
                .CompareElements(&self.element, &focused)
                .map_err(error)?
                .as_bool()
                || focused.CurrentProcessId().map_err(error)? as u32 != self.pid
                || focused.CurrentIsPassword().map_err(error)?.as_bool()
                || !focused.CurrentHasKeyboardFocus().map_err(error)?.as_bool()
            {
                return Err("Destination focus changed or became secure.".into());
            }
            if !self.terminal {
                if let (Some(value), Some(expected)) = (&self.value, &self.expected)
                    && !value
                        .CurrentValue()
                        .map_err(error)?
                        .iter()
                        .copied()
                        .eq(expected.encode_utf16())
                {
                    return Err("Destination was edited outside dictation.".into());
                }
                if let Some(previous) = &self.selection {
                    let current =
                        selection(&focused)?.ok_or("Destination cursor is unavailable")?;
                    if !previous.Compare(&current).map_err(error)?.as_bool() {
                        return Err("Destination cursor moved.".into());
                    }
                }
            }
            Ok(())
        }
    }
    pub fn insert(&mut self, text: &str, cancel: &AtomicBool) -> Result<(), String> {
        let result = self.insert_inner(text, cancel);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn insert_inner(&mut self, text: &str, cancel: &AtomicBool) -> Result<(), String> {
        self.check()?;
        let text = crate::desktop::keyboard_text(text);
        for c in text.chars() {
            if cancel.load(Ordering::Acquire) {
                return Err("Dictation cancelled. Already typed text was retained.".into());
            }
            // Revalidate focus for every character. Keep surrogate pairs together.
            unsafe {
                if modifiers_down()
                    || GetForegroundWindow() != self.window
                    || !self
                        .automation
                        .CompareElements(
                            &self.element,
                            &self.automation.GetFocusedElement().map_err(error)?,
                        )
                        .map_err(error)?
                        .as_bool()
                {
                    return Err("Focus or keyboard modifiers changed.".into());
                }
                let mut utf16 = [0u16; 2];
                let mut inputs = Vec::new();
                for unit in c.encode_utf16(&mut utf16) {
                    for flags in [KEYEVENTF_UNICODE, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP] {
                        inputs.push(INPUT {
                            r#type: INPUT_KEYBOARD,
                            Anonymous: INPUT_0 {
                                ki: KEYBDINPUT {
                                    wScan: *unit,
                                    dwFlags: flags,
                                    ..Default::default()
                                },
                            },
                        });
                    }
                }
                let sent = SendInput(&inputs, size_of::<INPUT>() as i32);
                if sent != inputs.len() as u32 {
                    // Release any potentially pressed packet key. Do not replay text.
                    let releases: Vec<_> = inputs
                        .iter()
                        .copied()
                        .filter(|input| input.Anonymous.ki.dwFlags.contains(KEYEVENTF_KEYUP))
                        .collect();
                    let _ = SendInput(&releases, size_of::<INPUT>() as i32);
                    return Err("Windows did not accept the complete Unicode input batch.".into());
                }
            }
        }
        if !self.terminal {
            if let (Some(value), Some(before)) = (&self.value, &self.expected) {
                let deadline = std::time::Instant::now() + Duration::from_millis(500);
                loop {
                    let after = unsafe { value.CurrentValue().map_err(error)?.to_string() };
                    if crate::desktop::is_insertion(before, &after, &text) {
                        self.expected = Some(after);
                        break;
                    }
                    if std::time::Instant::now() >= deadline {
                        return Err(
                            "Typed text could not be acknowledged. No retry will be sent.".into(),
                        );
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            } else {
                std::thread::sleep(Duration::from_millis(30));
            }
            self.selection = selection(&self.element)?;
        }
        Ok(())
    }
}
pub fn modifiers_down() -> bool {
    unsafe {
        [VK_CONTROL, VK_SHIFT, VK_MENU, VK_LWIN, VK_RWIN]
            .iter()
            .any(|key| GetAsyncKeyState(key.0 as i32) < 0)
    }
}
pub fn status() -> super::DesktopStatus {
    super::DesktopStatus { supported: true, keyboard_ready: true, permission_required: false, platform: "Windows", message: "UI Automation + Unicode input. Password and elevated fields are refused. Windows hardware verification is still required.".into() }
}
pub fn prepare() -> Result<(), String> {
    let _apartment = Apartment::open()?;
    // Verify the accessibility client can actually be initialized. This does
    // not claim every destination supports UIA or authorize elevated targets.
    let _: IUIAutomation2 =
        unsafe { CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER) }.map_err(error)?;
    Ok(())
}
