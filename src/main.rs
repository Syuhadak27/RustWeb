//! RustWeb — single-account WhatsApp Web desktop client.
//! Pure Rust + Chromium Embedded Framework. No Tauri, no WKWebView, no updater.

use cef::{args::Args, rc::*, sandbox_info::SandboxInfo, *};
use std::ffi::c_int;
use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, NSObject, Sel};
use objc2::{msg_send, sel, AnyThread, ClassType, MainThreadMarker};
use objc2_app_kit::{
    NSEventModifierFlags, NSApplication, NSImage, NSMenu, NSMenuItem, NSStatusItem, NSStatusBar, NSWindow,
    NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{NSString, NSUserNotification, NSUserNotificationCenter};
use std::process::Command;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Arc, Mutex};

const WHATSAPP_URL: &str = "https://web.whatsapp.com";
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 13_6_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
const WINDOW_WIDTH: i32 = 1180;
const WINDOW_HEIGHT: i32 = 800;
const NOTIFICATION_MESSAGE: &str = "RustWebNotification";

/// JS injected into the page: advertise granted notification permission and
/// forward every `new Notification(...)` into the native Rust bridge.
const NOTIFY_JS: &str = r#"
(function () {
  if (window.__rustwebNotifyBridge) return;
  window.__rustwebNotifyBridge = true;
  var DEDUP_MS = 3500;
  var recent = Object.create(null);
  var Bridge = function () {};
  Bridge.permission = 'granted';
  Bridge.requestPermission = function () { return Promise.resolve('granted'); };
  var Original = window.Notification;
  function Shim(title, options) {
    if (!(this instanceof Shim)) return new Shim(title, options);
    var t = String(title || 'WhatsApp');
    var b = String((options && options.body) || '');
    var key = t + ' ' + b;
    var now = Date.now();
    for (var k in recent) { if (now - recent[k] > DEDUP_MS) delete recent[k]; }
    if (recent[key] && now - recent[key] <= DEDUP_MS) return;
    recent[key] = now;
    try { window.rustwebNotify(t, b); } catch (e) {}
  }
  Shim.prototype = Original ? Original.prototype : {};
  Shim.permission = 'granted';
  Shim.requestPermission = function () { return Promise.resolve('granted'); };
  window.Notification = Shim;
  try {
    Object.defineProperty(window.Notification, 'permission', {
      configurable: true, get: function () { return 'granted'; }
    });
  } catch (e) {}
})();
"#;

/// Single, permanent session directory. No CLI profile switching.
fn cache_dir() -> String {
    let home = dirs::home_dir().expect("cannot locate the user home directory");
    let path = home.join("Library/Application Support/RustWeb/profile");
    std::fs::create_dir_all(&path).expect("cannot create the session directory");
    path.to_str()
        .expect("session path is not valid UTF-8")
        .to_string()
}

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
            let _ = writeln!(file, "[{}] {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"), message);
        }
    }
}

fn is_whatsapp(url: &str) -> bool {
    url.starts_with(WHATSAPP_URL) || url.starts_with("http://web.whatsapp.com")
}

/// Open anything outside WhatsApp Web in the default macOS browser.
fn open_external(url: &str) {
    if url.is_empty() || is_whatsapp(url) {
        return;
    }
    let _ = Command::new("open").arg(url).spawn();
}

/// Shared window handle. Stored as a raw pointer so AppKit callbacks can
/// restore the window from the menu bar.
static APP_STATE: AtomicPtr<AppState> = AtomicPtr::new(std::ptr::null_mut());

struct AppState {
    window: Mutex<Option<Window>>,
}

impl AppState {
    fn new() -> Self {
        Self {
            window: Mutex::new(None),
        }
    }

    fn show_window(&self) {
        if let Ok(guard) = self.window.lock() {
            if let Some(window) = guard.as_ref() {
                window.set_visible(1);
            }
        }
    }
}

fn app_state() -> Option<&'static AppState> {
    let ptr = APP_STATE.load(Ordering::SeqCst);
    if ptr.is_null() {
        None
    } else {
        Some(unsafe { &*ptr })
    }
}

// ---------------------------------------------------------------------------
// Native macOS menu bar icon + Notification Center helpers
// ---------------------------------------------------------------------------

static mut STATUS_ITEM: Option<objc2::rc::Retained<NSStatusItem>> = None;

extern "C-unwind" fn toggle_window(_this: &NSObject, _cmd: Sel) {
    if let Some(state) = app_state() {
        state.show_window();
    }
}

extern "C-unwind" fn quit_app(_this: &NSObject, _cmd: Sel, _sender: Option<&AnyObject>) {
    if let Some(mtm) = MainThreadMarker::new() {
        NSApplication::sharedApplication(mtm).terminate(None);
    }
}

extern "C-unwind" fn toggle_full_screen(
    _this: &NSObject,
    _cmd: Sel,
    _sender: Option<&AnyObject>,
) {
    if let Some(state) = app_state() {
        if let Ok(guard) = state.window.lock() {
            if let Some(window) = guard.as_ref() {
                if let Some(ns_window) = ns_window_from_cef(window) {
                    ns_window.toggleFullScreen(None);
                }
            }
        }
    }
}

fn tray_target_class() -> &'static AnyClass {
    let mut builder = ClassBuilder::new(c"RustWebTrayTarget", NSObject::class())
        .expect("failed to create the menu bar target class");
    unsafe {
        builder.add_method(
            sel!(toggleWindow),
            toggle_window as extern "C-unwind" fn(_, _),
        );
        builder.add_method(
            sel!(quitApp:),
            quit_app as extern "C-unwind" fn(_, _, _),
        );
        builder.add_method(
            sel!(toggleFullScreen:),
            toggle_full_screen as extern "C-unwind" fn(_, _, _),
        );
    }
    builder.register()
}

/// Resolve the native NSWindow backing a CEF Views window handle.
fn ns_window_from_cef(window: &impl ImplWindow) -> Option<&NSWindow> {
    let handle = window.get_window_handle();
    if handle.is_null() {
        return None;
    }
    Some(unsafe { &*(handle as *const NSWindow) })
}

/// Make the native window capable of macOS Full Screen (green traffic light).
fn configure_fullscreen(window: &impl ImplWindow) {
    let Some(ns_window) = ns_window_from_cef(window) else { return };
    let mut style = ns_window.styleMask();
    style.insert(NSWindowStyleMask::Resizable);
    ns_window.setStyleMask(style);
    let mut behavior = ns_window.collectionBehavior();
    behavior.insert(NSWindowCollectionBehavior::FullScreenPrimary);
    ns_window.setCollectionBehavior(behavior);
}

/// Sync native fullscreen state with the state requested by the web page.
fn ensure_native_fullscreen(fullscreen: bool) {
    if let Some(state) = app_state() {
        if let Ok(guard) = state.window.lock() {
            if let Some(window) = guard.as_ref() {
                if let Some(ns_window) = ns_window_from_cef(window) {
                    let is_fullscreen = ns_window
                        .styleMask()
                        .contains(NSWindowStyleMask::FullScreen);
                    if is_fullscreen != fullscreen {
                        ns_window.toggleFullScreen(None);
                    }
                }
            }
        }
    }
}

/// Application menu: View → Enter Full Screen (⌃⌘F), and Quit.
fn install_app_menu() {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let main_menu = NSMenu::new(mtm);
    
    // App menu (RustWeb)
    let app_item = NSMenuItem::new(mtm);
    let app_menu = NSMenu::new(mtm);
    let quit_item = unsafe {
        app_menu.addItemWithTitle_action_keyEquivalent(
            &NSString::from_str("Quit RustWeb"),
            Some(sel!(quitApp:)),
            &NSString::from_str("q"),
        )
    };
    quit_item.setKeyEquivalentModifierMask(NSEventModifierFlags::Command);
    app_item.setSubmenu(Some(&app_menu));
    main_menu.addItem(&app_item);

    // View menu
    let view_item = NSMenuItem::new(mtm);
    view_item.setTitle(&NSString::from_str("View"));
    let view_menu = NSMenu::new(mtm);
    let fullscreen_item = unsafe {
        view_menu.addItemWithTitle_action_keyEquivalent(
            &NSString::from_str("Enter Full Screen"),
            Some(sel!(toggleFullScreen:)),
            &NSString::from_str("f"),
        )
    };
    fullscreen_item.setKeyEquivalentModifierMask(
        NSEventModifierFlags::Control | NSEventModifierFlags::Command,
    );
    view_item.setSubmenu(Some(&view_menu));
    main_menu.addItem(&view_item);

    NSApplication::sharedApplication(mtm).setMainMenu(Some(&main_menu));
}

fn resource_path(name: &str) -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    exe.parent()?
        .parent()?
        .join("Resources")
        .join(name)
        .to_str()
        .map(str::to_string)
}

fn tray_image(name: &str) -> Option<objc2::rc::Retained<NSImage>> {
    let path = resource_path(name)?;
    let image = NSImage::initWithContentsOfFile(NSImage::alloc(), &NSString::from_str(&path));
    let image = image?;
    image.setTemplate(true);
    Some(image)
}

fn set_tray_unread(unread: bool) {
    let name = if unread {
        "tray-template-unread.png"
    } else {
        "tray-template.png"
    };
    let Some(image) = tray_image(name) else { return };
    let Some(mtm) = MainThreadMarker::new() else { return };
    #[allow(static_mut_refs)]
    unsafe {
        if let Some(item) = STATUS_ITEM.as_ref() {
            if let Some(button) = item.button(mtm) {
                button.setImage(Some(&image));
            }
        }
    }
}

fn install_tray() {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let item = NSStatusBar::systemStatusBar().statusItemWithLength(24.0);
    let target: objc2::rc::Retained<AnyObject> = unsafe { msg_send![tray_target_class(), new] };
    if let Some(button) = item.button(mtm) {
        if let Some(image) = tray_image("tray-template.png") {
            button.setImage(Some(&image));
        }
        unsafe {
            button.setTarget(Some(&target));
            button.setAction(Some(sel!(toggleWindow)));
        }
    }
    // Right-click menu with Quit
    let menu = NSMenu::new(mtm);
    let show_item = NSMenuItem::new(mtm);
    show_item.setTitle(&NSString::from_str("Show RustWeb"));
    unsafe {
        show_item.setTarget(Some(&target));
        show_item.setAction(Some(sel!(toggleWindow)));
    }
    menu.addItem(&show_item);
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    let quit_item = NSMenuItem::new(mtm);
    quit_item.setTitle(&NSString::from_str("Quit RustWeb"));
    unsafe {
        quit_item.setTarget(Some(&target));
        quit_item.setAction(Some(sel!(quitApp:)));
    }
    menu.addItem(&quit_item);
    item.setMenu(Some(&menu));
    
    #[allow(static_mut_refs)]
    unsafe {
        STATUS_ITEM = Some(item);
    }
}

/// Notification using NSUserNotification (compatible with macOS 13+)
#[allow(deprecated)]
fn native_notify(title: &str, body: &str) {
    let notification = NSUserNotification::new();
    notification.setTitle(Some(&NSString::from_str(title)));
    notification.setInformativeText(Some(&NSString::from_str(body)));
    notification.setSoundName(None);
    NSUserNotificationCenter::defaultUserNotificationCenter().deliverNotification(&notification);
    set_tray_unread(true);
}

// ---------------------------------------------------------------------------
// CEF handlers
// ---------------------------------------------------------------------------

struct DemoApp {
    object: *mut RcImpl<cef::sys::_cef_app_t, Self>,
}

impl DemoApp {
    fn new() -> App {
        App::new(Self {
            object: std::ptr::null_mut(),
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
        Self { object }
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
            // WebRTC media access without a blocking permission prompt.
            command_line.append_switch(Some(&"use-fake-ui-for-media-stream".into()));
            command_line.append_switch(Some(&"enable-media-stream".into()));
            // Keep WhatsApp's WebSocket alive while the window is hidden.
            command_line.append_switch(Some(&"disable-background-timer-throttling".into()));
            command_line.append_switch(Some(&"disable-backgrounding-occluded-windows".into()));
            command_line.append_switch(Some(&"disable-renderer-backgrounding".into()));
            // Performance optimizations
            command_line.append_switch(Some(&"disable-gpu-vsync".into()));
            command_line.append_switch(Some(&"enable-gpu-rasterization".into()));
            command_line.append_switch(Some(&"enable-zero-copy".into()));
            // Reduce memory usage
            command_line.append_switch_with_value(Some(&"max-active-renderer-processes".into()), Some(&"4".into()));
            command_line.append_switch(Some(&"disable-site-isolation-trials".into()));
            command_line.append_switch_with_value(Some(&"disable-features".into()), Some(&"CalculateNativeWinOcclusion,InterestFeedContentSuggestions".into()));
        }
    }

    fn get_browser_process_handler(&self) -> Option<BrowserProcessHandler> {
        Some(DemoBrowserProcessHandler::new())
    }

    fn get_render_process_handler(&self) -> Option<RenderProcessHandler> {
        Some(DemoRenderProcessHandler::new())
    }
}

struct DemoBrowserProcessHandler {
    object: *mut RcImpl<cef::sys::cef_browser_process_handler_t, Self>,
}

impl DemoBrowserProcessHandler {
    fn new() -> BrowserProcessHandler {
        BrowserProcessHandler::new(Self {
            object: std::ptr::null_mut(),
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
        Self { object }
    }
}

impl ImplBrowserProcessHandler for DemoBrowserProcessHandler {
    fn get_raw(&self) -> *mut cef::sys::cef_browser_process_handler_t {
        self.object.cast()
    }

    fn on_context_initialized(&self) {
        log("CEF context initialized");
        let mut client = DemoClient::new();
        let url = CefString::from(&CefStringUtf8::from(WHATSAPP_URL));

        let mut context_settings = RequestContextSettings::default();
        context_settings.cache_path = cache_dir().as_str().into();
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
        if let Some(state) = app_state() {
            if let Ok(mut window) = state.window.lock() {
                *window = Some(
                    window_create_top_level(Some(&mut delegate)).expect("failed to create the window"),
                );
                log("top-level window created");
            }
        }
    }
}

// -- Permission handler: grant camera/microphone immediately -----------------

struct WebPermissionHandler(*mut RcImpl<cef::sys::_cef_permission_handler_t, Self>);

impl WebPermissionHandler {
    fn new() -> PermissionHandler {
        PermissionHandler::new(Self(std::ptr::null_mut()))
    }
}

impl WrapPermissionHandler for WebPermissionHandler {
    fn wrap_rc(
        &mut self,
        object: *mut RcImpl<cef::sys::_cef_permission_handler_t, Self>,
    ) {
        self.0 = object;
    }
}

impl Clone for WebPermissionHandler {
    fn clone(&self) -> Self {
        unsafe {
            let rc_impl = &mut *self.0;
            rc_impl.interface.add_ref();
        }
        Self(self.0)
    }
}

impl Rc for WebPermissionHandler {
    fn as_base(&self) -> &cef::sys::cef_base_ref_counted_t {
        unsafe {
            let base = &*self.0;
            std::mem::transmute(&base.cef_object)
        }
    }
}

impl ImplPermissionHandler for WebPermissionHandler {
    fn get_raw(&self) -> *mut cef::sys::_cef_permission_handler_t {
        self.0 as *mut cef::sys::_cef_permission_handler_t
    }

    fn on_request_media_access_permission(
        &self,
        _browser: Option<&mut impl ImplBrowser>,
        _frame: Option<&mut impl ImplFrame>,
        _requesting_origin: Option<&CefStringUtf16>,
        requested_permissions: u32,
        callback: Option<&mut impl ImplMediaAccessCallback>,
    ) -> c_int {
        if let Some(callback) = callback {
            callback.cont(requested_permissions);
        }
        1
    }
}

// -- Life span handler: external links go to the default browser -------------

struct AppLifeSpanHandler(*mut RcImpl<cef::sys::_cef_life_span_handler_t, Self>);

impl AppLifeSpanHandler {
    fn new() -> LifeSpanHandler {
        LifeSpanHandler::new(Self(std::ptr::null_mut()))
    }
}

impl WrapLifeSpanHandler for AppLifeSpanHandler {
    fn wrap_rc(&mut self, object: *mut RcImpl<cef::sys::_cef_life_span_handler_t, Self>) {
        self.0 = object;
    }
}

impl Clone for AppLifeSpanHandler {
    fn clone(&self) -> Self {
        unsafe {
            let rc_impl = &mut *self.0;
            rc_impl.interface.add_ref();
        }
        Self(self.0)
    }
}

impl Rc for AppLifeSpanHandler {
    fn as_base(&self) -> &cef::sys::cef_base_ref_counted_t {
        unsafe {
            let base = &*self.0;
            std::mem::transmute(&base.cef_object)
        }
    }
}

impl ImplLifeSpanHandler for AppLifeSpanHandler {
    fn get_raw(&self) -> *mut cef::sys::_cef_life_span_handler_t {
        self.0 as *mut cef::sys::_cef_life_span_handler_t
    }

    fn on_before_popup(
        &self,
        _browser: Option<&mut impl ImplBrowser>,
        _frame: Option<&mut impl ImplFrame>,
        _popup_id: c_int,
        target_url: Option<&CefStringUtf16>,
        _target_frame_name: Option<&CefStringUtf16>,
        _target_disposition: WindowOpenDisposition,
        _user_gesture: c_int,
        _popup_features: Option<&PopupFeatures>,
        _window_info: Option<&mut WindowInfo>,
        _client: Option<&mut impl ImplClient>,
        _settings: Option<&mut BrowserSettings>,
        _extra_info: Option<&mut impl ImplDictionaryValue>,
        _no_javascript_access: Option<&mut c_int>,
    ) -> c_int {
        if let Some(url) = target_url {
            let url = CefStringUtf8::from(url).to_string();
            if !is_whatsapp(&url) {
                open_external(&url);
                return 1;
            }
        }
        0
    }
}

// -- Request handler: cancel navigation to external URLs ---------------------

struct AppRequestHandler(*mut RcImpl<cef::sys::_cef_request_handler_t, Self>);

impl AppRequestHandler {
    fn new() -> RequestHandler {
        RequestHandler::new(Self(std::ptr::null_mut()))
    }
}

impl WrapRequestHandler for AppRequestHandler {
    fn wrap_rc(&mut self, object: *mut RcImpl<cef::sys::_cef_request_handler_t, Self>) {
        self.0 = object;
    }
}

impl Clone for AppRequestHandler {
    fn clone(&self) -> Self {
        unsafe {
            let rc_impl = &mut *self.0;
            rc_impl.interface.add_ref();
        }
        Self(self.0)
    }
}

impl Rc for AppRequestHandler {
    fn as_base(&self) -> &cef::sys::cef_base_ref_counted_t {
        unsafe {
            let base = &*self.0;
            std::mem::transmute(&base.cef_object)
        }
    }
}

impl ImplRequestHandler for AppRequestHandler {
    fn get_raw(&self) -> *mut cef::sys::_cef_request_handler_t {
        self.0 as *mut cef::sys::_cef_request_handler_t
    }

    fn on_before_browse(
        &self,
        _browser: Option<&mut impl ImplBrowser>,
        _frame: Option<&mut impl ImplFrame>,
        request: Option<&mut impl ImplRequest>,
        _user_gesture: c_int,
        _is_redirect: c_int,
    ) -> c_int {
        if let Some(request) = request {
            if let Some(url) = request.get_url() {
                let url = CefStringUtf8::from(&url).to_string();
                if !is_whatsapp(&url) {
                    open_external(&url);
                    return 1;
                }
            }
        }
        0
    }
}

// -- Display handler: web page fullscreen -> native macOS fullscreen ---------

struct DemoDisplayHandler(*mut RcImpl<cef::sys::_cef_display_handler_t, Self>);

impl DemoDisplayHandler {
    fn new() -> DisplayHandler {
        DisplayHandler::new(Self(std::ptr::null_mut()))
    }
}

impl WrapDisplayHandler for DemoDisplayHandler {
    fn wrap_rc(&mut self, object: *mut RcImpl<cef::sys::_cef_display_handler_t, Self>) {
        self.0 = object;
    }
}

impl Clone for DemoDisplayHandler {
    fn clone(&self) -> Self {
        unsafe {
            let rc_impl = &mut *self.0;
            rc_impl.interface.add_ref();
        }
        Self(self.0)
    }
}

impl Rc for DemoDisplayHandler {
    fn as_base(&self) -> &cef::sys::cef_base_ref_counted_t {
        unsafe {
            let base = &*self.0;
            std::mem::transmute(&base.cef_object)
        }
    }
}

impl ImplDisplayHandler for DemoDisplayHandler {
    fn get_raw(&self) -> *mut cef::sys::_cef_display_handler_t {
        self.0 as *mut cef::sys::_cef_display_handler_t
    }

    fn on_fullscreen_mode_change(
        &self,
        _browser: Option<&mut impl ImplBrowser>,
        fullscreen: c_int,
    ) {
        ensure_native_fullscreen(fullscreen != 0);
    }
}

// -- Renderer bridge: Notification -> process message ------------------------

struct NotifyV8Handler {
    object: *mut RcImpl<cef::sys::_cef_v8handler_t, Self>,
    frame: Option<Frame>,
}

impl NotifyV8Handler {
    fn new(frame: Option<Frame>) -> V8handler {
        V8handler::new(Self {
            object: std::ptr::null_mut(),
            frame,
        })
    }
}

impl WrapV8handler for NotifyV8Handler {
    fn wrap_rc(&mut self, object: *mut RcImpl<cef::sys::_cef_v8handler_t, Self>) {
        self.object = object;
    }
}

impl Clone for NotifyV8Handler {
    fn clone(&self) -> Self {
        unsafe {
            let rc_impl = &mut *self.object;
            rc_impl.interface.add_ref();
        }
        Self {
            object: self.object,
            frame: self.frame.clone(),
        }
    }
}

impl Rc for NotifyV8Handler {
    fn as_base(&self) -> &cef::sys::cef_base_ref_counted_t {
        unsafe {
            let base = &*self.object;
            std::mem::transmute(&base.cef_object)
        }
    }
}

impl ImplV8handler for NotifyV8Handler {
    fn get_raw(&self) -> *mut cef::sys::_cef_v8handler_t {
        self.object as *mut cef::sys::_cef_v8handler_t
    }

    fn execute(
        &self,
        _name: Option<&CefStringUtf16>,
        _object: Option<&mut impl ImplV8value>,
        arguments: Option<&[Option<impl ImplV8value>]>,
        _retval: Option<&mut impl ImplV8value>,
        _exception: Option<&mut CefStringUtf16>,
    ) -> c_int {
        let arguments = arguments.unwrap_or(&[]);
        let title = arguments
            .first()
            .and_then(|value| value.as_ref())
            .and_then(|value| value.get_string_value())
            .map(|value| CefStringUtf8::from(&value).to_string())
            .unwrap_or_else(|| "WhatsApp".to_string());
        let body = arguments
            .get(1)
            .and_then(|value| value.as_ref())
            .and_then(|value| value.get_string_value())
            .map(|value| CefStringUtf8::from(&value).to_string())
            .unwrap_or_default();

        if let Some(frame) = &self.frame {
            if let Some(mut message) = process_message_create(Some(&NOTIFICATION_MESSAGE.into())) {
                if let Some(args) = message.get_argument_list() {
                    args.set_string(0, Some(&title.as_str().into()));
                    args.set_string(1, Some(&body.as_str().into()));
                }
                frame.send_process_message(ProcessId::default(), Some(&mut message));
            }
        }
        1
    }
}

struct DemoRenderProcessHandler {
    object: *mut RcImpl<cef::sys::_cef_render_process_handler_t, Self>,
}

impl DemoRenderProcessHandler {
    fn new() -> RenderProcessHandler {
        RenderProcessHandler::new(Self {
            object: std::ptr::null_mut(),
        })
    }
}

impl WrapRenderProcessHandler for DemoRenderProcessHandler {
    fn wrap_rc(
        &mut self,
        object: *mut RcImpl<cef::sys::_cef_render_process_handler_t, Self>,
    ) {
        self.object = object;
    }
}

impl Clone for DemoRenderProcessHandler {
    fn clone(&self) -> Self {
        unsafe {
            let rc_impl = &mut *self.object;
            rc_impl.interface.add_ref();
        }
        Self { object: self.object }
    }
}

impl Rc for DemoRenderProcessHandler {
    fn as_base(&self) -> &cef::sys::cef_base_ref_counted_t {
        unsafe {
            let base = &*self.object;
            std::mem::transmute(&base.cef_object)
        }
    }
}

impl ImplRenderProcessHandler for DemoRenderProcessHandler {
    fn get_raw(&self) -> *mut cef::sys::_cef_render_process_handler_t {
        self.object as *mut cef::sys::_cef_render_process_handler_t
    }

    fn on_context_created(
        &self,
        _browser: Option<&mut impl ImplBrowser>,
        frame: Option<&mut impl ImplFrame>,
        context: Option<&mut impl ImplV8context>,
    ) {
        let (Some(_frame), Some(context)) = (frame, context) else { return };
        let Some(frame) = context.get_frame() else { return };
        if frame.is_main() != 1 {
            return;
        }
        let Some(global) = context.get_global() else { return };
        let mut handler = NotifyV8Handler::new(Some(frame.clone()));
        let name = CefStringUtf16::from("rustwebNotify");
        if let Some(mut function) = v8value_create_function(Some(&name), Some(&mut handler)) {
            global.set_value_bykey(
                Some(&name),
                Some(&mut function),
                V8Propertyattribute::default(),
            );
        }
        frame.execute_java_script(Some(&CefStringUtf16::from(NOTIFY_JS)), None, 0);
    }
}

// -- Client: handlers + native notification delivery -------------------------

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

    fn get_permission_handler(&self) -> Option<PermissionHandler> {
        Some(WebPermissionHandler::new())
    }

    fn get_life_span_handler(&self) -> Option<LifeSpanHandler> {
        Some(AppLifeSpanHandler::new())
    }

    fn get_request_handler(&self) -> Option<RequestHandler> {
        Some(AppRequestHandler::new())
    }

    fn get_display_handler(&self) -> Option<DisplayHandler> {
        Some(DemoDisplayHandler::new())
    }

    fn on_process_message_received(
        &self,
        _browser: Option<&mut impl ImplBrowser>,
        _frame: Option<&mut impl ImplFrame>,
        _source_process: ProcessId,
        message: Option<&mut impl ImplProcessMessage>,
    ) -> c_int {
        let Some(message) = message else { return 0 };
        let name = message
            .get_name()
            .map(|name| CefStringUtf8::from(&name).to_string())
            .unwrap_or_default();
        if name != NOTIFICATION_MESSAGE {
            return 0;
        }
        let args = message.get_argument_list();
        let title = args
            .as_ref()
            .and_then(|args| args.get_string(0))
            .map(|value| CefStringUtf8::from(&value).to_string())
            .unwrap_or_else(|| "WhatsApp".to_string());
        let body = args
            .as_ref()
            .and_then(|args| args.get_string(1))
            .map(|value| CefStringUtf8::from(&value).to_string())
            .unwrap_or_default();
        native_notify(&title, &body);
        1
    }
}

// -- Window delegate: hide on close, restore from menu bar -------------------

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
            configure_fullscreen(window);
            let mut view = self.browser_view.clone();
            window.add_child_view(Some(&mut view));
            window.show();
        }
    }

    fn on_window_destroyed(&self, _window: Option<&mut impl ImplWindow>) {
        log("window destroyed — quitting message loop");
        quit_message_loop();
    }

    /// Close button hides the window to the menu bar instead of quitting.
    fn can_close(&self, window: Option<&mut impl ImplWindow>) -> c_int {
        if let Some(window) = window {
            window.set_visible(0);
        }
        0
    }

    fn with_standard_window_buttons(
        &self,
        _window: Option<&mut impl ImplWindow>,
    ) -> c_int {
        1
    }

    fn can_resize(&self, _window: Option<&mut impl ImplWindow>) -> c_int {
        1
    }

    fn can_maximize(&self, _window: Option<&mut impl ImplWindow>) -> c_int {
        1
    }

    fn can_minimize(&self, _window: Option<&mut impl ImplWindow>) -> c_int {
        1
    }
}

fn main() {
    std::panic::set_hook(Box::new(|info| log(&format!("PANIC: {}", info))));
    log("rustweb starting");

    let state = Arc::new(AppState::new());
    APP_STATE.store(Arc::into_raw(state.clone()) as *mut AppState, Ordering::SeqCst);
    install_tray();
    install_app_menu();

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

    let mut app = DemoApp::new();
    let ret = execute_process(
        Some(args.as_main_args()),
        Some(&mut app),
        sandbox.as_mut_ptr(),
    );
    log(&format!("execute_process returned {ret}"));

    if !is_browser_process {
        return;
    }
    assert_eq!(ret, -1, "cannot execute browser process");

    let mut settings = Settings::default();
    settings.no_sandbox = 1;
    settings.root_cache_path = cache_dir().as_str().into();
    settings.persist_session_cookies = 1;
    settings.user_agent = USER_AGENT.into();
    // Performance & memory optimizations (only fields that exist in CEF 132)
    settings.multi_threaded_message_loop = 1;
    settings.windowless_rendering_enabled = 0;
    settings.command_line_args_disabled = 0;
    settings.cache_path = cache_dir().as_str().into();
    settings.uncaught_exception_stack_size = 10;
    settings.background_color = 0xFFFFFFFF;

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
    
    shutdown();
    log("CEF shutdown complete");

    // Reclaim the Arc so it drops cleanly (prevents the original memory leak)
    let raw = APP_STATE.swap(std::ptr::null_mut(), Ordering::SeqCst);
    if !raw.is_null() {
        unsafe { drop(Arc::from_raw(raw as *const AppState)); }
    }
}