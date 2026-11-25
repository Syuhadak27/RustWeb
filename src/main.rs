//! RustWeb — a minimal WhatsApp Web desktop client built with Rust + CEF.

use cef::{args::Args, rc::*, sandbox_info::SandboxInfo, *};
use std::sync::{Arc, Mutex};

const WHATSAPP_URL: &str = "https://web.whatsapp.com";
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 13_6_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
const WINDOW_WIDTH: i32 = 1180;
const WINDOW_HEIGHT: i32 = 800;

/// Small file logger so a macOS launch failure can be diagnosed without a console.
fn log(message: &str) {
    if let Some(home) = dirs::home_dir() {
        let dir = home.join("Library/Application Support/RustWeb");
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("launch.log"))
        {
            use std::io::Write;
            let _ = writeln!(file, "{}", message);
        }
    }
}

/// Parsed `--profile=<name>` argument and the resulting cache directory.
struct AppState {
    profile_dir: String,
    window: Mutex<Option<Window>>,
}

impl AppState {
    fn new() -> Self {
        let profile = std::env::args()
            .skip(1)
            .find_map(|arg| arg.strip_prefix("--profile=").map(str::to_string))
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "default".to_string());

        let home = dirs::home_dir().expect("cannot locate the user home directory");
        let profile_dir = home
            .join("Library/Application Support/RustWeb/profiles")
            .join(profile);
        std::fs::create_dir_all(&profile_dir).expect("cannot create the profile directory");

        Self {
            profile_dir: profile_dir
                .to_str()
                .expect("profile path is not valid UTF-8")
                .to_string(),
            window: Mutex::new(None),
        }
    }
}

struct DemoApp {
    object: *mut RcImpl<cef::sys::_cef_app_t, Self>,
    state: Arc<AppState>,
}

impl DemoApp {
    fn new(state: Arc<AppState>) -> App {
        App::new(Self {
            object: std::ptr::null_mut(),
            state,
        })
    }
}

impl WrapApp for DemoApp {
    fn wrap_rc(&mut self, object: *mut RcImpl<cef::sys::_cef_app_t, Self>) {
        self.object = object;
    }
}

impl Clone for DemoApp {
    fn clone(&self) -> Self {
        let object = unsafe {
            let rc_impl = &mut *self.object;
            rc_impl.interface.add_ref();
            self.object
        };
        Self {
            object,
            state: self.state.clone(),
        }
    }
}

impl Rc for DemoApp {
    fn as_base(&self) -> &cef::sys::cef_base_ref_counted_t {
        unsafe {
            let base = &*self.object;
            std::mem::transmute(&base.cef_object)
        }
    }
}

impl ImplApp for DemoApp {
    fn get_raw(&self) -> *mut cef::sys::_cef_app_t {
        self.object as *mut cef::sys::_cef_app_t
    }

    fn on_before_command_line_processing(
        &self,
        _process_type: Option<&CefStringUtf16>,
        command_line: Option<&mut impl ImplCommandLine>,
    ) {
        if let Some(command_line) = command_line {
            // Give WebRTC video/voice calls access to the camera and microphone
            // without a blocked permission prompt.
            command_line.append_switch(Some(&"use-fake-ui-for-media-stream".into()));
            command_line.append_switch(Some(&"enable-media-stream".into()));
        }
    }

    fn get_browser_process_handler(&self) -> Option<BrowserProcessHandler> {
        Some(DemoBrowserProcessHandler::new(self.state.clone()))
    }
}

struct DemoBrowserProcessHandler {
    object: *mut RcImpl<cef::sys::cef_browser_process_handler_t, Self>,
    state: Arc<AppState>,
}

impl DemoBrowserProcessHandler {
    fn new(state: Arc<AppState>) -> BrowserProcessHandler {
        BrowserProcessHandler::new(Self {
            object: std::ptr::null_mut(),
            state,
        })
    }
}

impl Rc for DemoBrowserProcessHandler {
    fn as_base(&self) -> &cef::sys::cef_base_ref_counted_t {
        unsafe {
            let base = &*self.object;
            std::mem::transmute(&base.cef_object)
        }
    }
}

impl WrapBrowserProcessHandler for DemoBrowserProcessHandler {
    fn wrap_rc(
        &mut self,
        object: *mut RcImpl<cef::sys::cef_browser_process_handler_t, Self>,
    ) {
        self.object = object;
    }
}

impl Clone for DemoBrowserProcessHandler {
    fn clone(&self) -> Self {
        let object = unsafe {
            let rc_impl = &mut *self.object;
            rc_impl.interface.add_ref();
            rc_impl
        };
        Self {
            object,
            state: self.state.clone(),
        }
    }
}

impl ImplBrowserProcessHandler for DemoBrowserProcessHandler {
    fn get_raw(&self) -> *mut cef::sys::cef_browser_process_handler_t {
        self.object.cast()
    }

    /// CEF objects must only be created after the context is initialized.
    fn on_context_initialized(&self) {
        log("CEF context initialized");
        let mut client = DemoClient::new();
        let url = CefString::from(&CefStringUtf8::from(WHATSAPP_URL));

        let mut context_settings = RequestContextSettings::default();
        context_settings.cache_path = self.state.profile_dir.as_str().into();
        context_settings.persist_session_cookies = 1;
        let mut context = request_context_create_context(
            Some(&context_settings),
            None::<&mut RequestContextHandler>,
        )
        .expect("failed to create the request context");

        let browser_view = browser_view_create(
            Some(&mut client),
            Some(&url),
            Some(&Default::default()),
            Option::<&mut DictionaryValue>::None,
            Some(&mut context),
            Option::<&mut BrowserViewDelegate>::None,
        )
        .expect("failed to create the browser view");

        let mut delegate = DemoWindowDelegate::new(browser_view);
        if let Ok(mut window) = self.state.window.lock() {
            *window = Some(
                window_create_top_level(Some(&mut delegate)).expect("failed to create the window"),
            );
            log("top-level window created");
        }
    }
}

struct DemoClient(*mut RcImpl<cef::sys::_cef_client_t, Self>);

impl DemoClient {
    fn new() -> Client {
        Client::new(Self(std::ptr::null_mut()))
    }
}

impl WrapClient for DemoClient {
    fn wrap_rc(&mut self, object: *mut RcImpl<cef::sys::_cef_client_t, Self>) {
        self.0 = object;
    }
}

impl Clone for DemoClient {
    fn clone(&self) -> Self {
        unsafe {
            let rc_impl = &mut *self.0;
            rc_impl.interface.add_ref();
        }
        Self(self.0)
    }
}

impl Rc for DemoClient {
    fn as_base(&self) -> &cef::sys::cef_base_ref_counted_t {
        unsafe {
            let base = &*self.0;
            std::mem::transmute(&base.cef_object)
        }
    }
}

impl ImplClient for DemoClient {
    fn get_raw(&self) -> *mut cef::sys::_cef_client_t {
        self.0 as *mut cef::sys::_cef_client_t
    }
}

struct DemoWindowDelegate {
    base: *mut RcImpl<cef::sys::_cef_window_delegate_t, Self>,
    browser_view: BrowserView,
}

impl DemoWindowDelegate {
    fn new(browser_view: BrowserView) -> WindowDelegate {
        WindowDelegate::new(Self {
            base: std::ptr::null_mut(),
            browser_view,
        })
    }
}

impl WrapWindowDelegate for DemoWindowDelegate {
    fn wrap_rc(&mut self, object: *mut RcImpl<cef::sys::_cef_window_delegate_t, Self>) {
        self.base = object;
    }
}

impl Clone for DemoWindowDelegate {
    fn clone(&self) -> Self {
        unsafe {
            let rc_impl = &mut *self.base;
            rc_impl.interface.add_ref();
        }
        Self {
            base: self.base,
            browser_view: self.browser_view.clone(),
        }
    }
}

impl Rc for DemoWindowDelegate {
    fn as_base(&self) -> &cef::sys::cef_base_ref_counted_t {
        unsafe {
            let base = &*self.base;
            std::mem::transmute(&base.cef_object)
        }
    }
}

impl ImplViewDelegate for DemoWindowDelegate {
    fn get_raw(&self) -> *mut cef::sys::_cef_view_delegate_t {
        self.base as *mut cef::sys::_cef_view_delegate_t
    }
}

impl ImplPanelDelegate for DemoWindowDelegate {}

impl ImplWindowDelegate for DemoWindowDelegate {
    fn on_window_created(&self, window: Option<&mut impl ImplWindow>) {
        log("window created");
        if let Some(window) = window {
            window.set_size(Some(&Size {
                width: WINDOW_WIDTH,
                height: WINDOW_HEIGHT,
            }));
            let mut view = self.browser_view.clone();
            window.add_child_view(Some(&mut view));
            window.show();
        }
    }

    fn on_window_destroyed(&self, _window: Option<&mut impl ImplWindow>) {
        log("window destroyed — quitting message loop");
        quit_message_loop();
    }

    fn with_standard_window_buttons(
        &self,
        _window: Option<&mut impl ImplWindow>,
    ) -> ::std::os::raw::c_int {
        1
    }

    fn can_resize(&self, _window: Option<&mut impl ImplWindow>) -> ::std::os::raw::c_int {
        1
    }

    fn can_maximize(&self, _window: Option<&mut impl ImplWindow>) -> ::std::os::raw::c_int {
        1
    }

    fn can_minimize(&self, _window: Option<&mut impl ImplWindow>) -> ::std::os::raw::c_int {
        1
    }

    fn can_close(&self, _window: Option<&mut impl ImplWindow>) -> ::std::os::raw::c_int {
        1
    }
}

fn main() {
    std::panic::set_hook(Box::new(|info| log(&format!("PANIC: {}", info))));
    log("rustweb starting");

    // Load Chromium Embedded Framework.framework from inside the .app bundle.
    #[cfg(target_os = "macos")]
    let _loader = {
        let loader = library_loader::LibraryLoader::new(&std::env::current_exe().unwrap(), false);
        assert!(loader.load(), "failed to load the Chromium Embedded Framework");
        log("CEF framework loaded");
        loader
    };

    let args = Args::new();
    let cmd = args.as_cmd_line().unwrap();
    let sandbox = SandboxInfo::new();
    let is_browser_process = cmd.has_switch(Some(&"type".into())) != 1;

    let state = Arc::new(AppState::new());
    let mut app = DemoApp::new(state.clone());

    let ret = execute_process(
        Some(args.as_main_args()),
        Some(&mut app),
        sandbox.as_mut_ptr(),
    );
    log(&format!("execute_process returned {ret}"));

    if !is_browser_process {
        // Renderer / GPU / utility helper processes exit here.
        return;
    }
    assert_eq!(ret, -1, "cannot execute browser process");

    let mut settings = Settings::default();
    // Ad-hoc signed bundles have no sandbox profile; let CEF run unsandboxed.
    settings.no_sandbox = 1;
    settings.root_cache_path = state.profile_dir.as_str().into();
    settings.persist_session_cookies = 1;
    settings.user_agent = USER_AGENT.into();

    assert_eq!(
        initialize(
            Some(args.as_main_args()),
            Some(&settings),
            Some(&mut app),
            sandbox.as_mut_ptr(),
        ),
        1,
        "failed to initialize CEF"
    );
    log("CEF initialized");

    run_message_loop();
    log("message loop exited");

    let window = state.window.lock().expect("failed to lock the window");
    let window = window.as_ref().expect("window was never created");
    assert!(window.has_one_ref());

    shutdown();
}
