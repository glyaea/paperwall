use http_range::HttpRange;
use serde::Deserialize;
use serde::Serialize;
use std::borrow::Cow;
use std::cell::RefCell;
use std::env;
use std::error::Error;
use std::fs;
use std::fs::File;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::mem;
use std::path::Path;
use std::path::PathBuf;
use std::rc::Rc;
use tao::dpi::LogicalSize;
use tao::dpi::PhysicalPosition;
use tao::dpi::PhysicalSize;
use tao::event::Event;
use tao::event::WindowEvent;
use tao::event_loop::ControlFlow;
use tao::event_loop::EventLoopBuilder;
use tao::event_loop::EventLoopProxy;
use tao::event_loop::EventLoopWindowTarget;
use tao::platform::windows::WindowBuilderExtWindows;
use tao::platform::windows::WindowExtWindows;
use tao::window::Window;
use tao::window::WindowBuilder;
use windows::Win32::Foundation::COLORREF;
use windows::Win32::Foundation::GetLastError;
use windows::Win32::Foundation::HWND;
use windows::Win32::Foundation::LPARAM;
use windows::Win32::Foundation::NO_ERROR;
use windows::Win32::Foundation::SetLastError;
use windows::Win32::Foundation::WPARAM;
use windows::Win32::UI::WindowsAndMessaging::EnumWindows;
use windows::Win32::UI::WindowsAndMessaging::FindWindowExW;
use windows::Win32::UI::WindowsAndMessaging::FindWindowW;
use windows::Win32::UI::WindowsAndMessaging::GWL_EXSTYLE;
use windows::Win32::UI::WindowsAndMessaging::GWL_STYLE;
use windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics;
use windows::Win32::UI::WindowsAndMessaging::GetWindowInfo;
use windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW;
use windows::Win32::UI::WindowsAndMessaging::HWND_BOTTOM;
use windows::Win32::UI::WindowsAndMessaging::LWA_ALPHA;
use windows::Win32::UI::WindowsAndMessaging::SM_CXVIRTUALSCREEN;
use windows::Win32::UI::WindowsAndMessaging::SM_CYVIRTUALSCREEN;
use windows::Win32::UI::WindowsAndMessaging::SM_XVIRTUALSCREEN;
use windows::Win32::UI::WindowsAndMessaging::SM_YVIRTUALSCREEN;
use windows::Win32::UI::WindowsAndMessaging::SMTO_NORMAL;
use windows::Win32::UI::WindowsAndMessaging::SWP_FRAMECHANGED;
use windows::Win32::UI::WindowsAndMessaging::SWP_NOACTIVATE;
use windows::Win32::UI::WindowsAndMessaging::SWP_SHOWWINDOW;
use windows::Win32::UI::WindowsAndMessaging::SendMessageTimeoutW;
use windows::Win32::UI::WindowsAndMessaging::SetLayeredWindowAttributes;
use windows::Win32::UI::WindowsAndMessaging::SetParent;
use windows::Win32::UI::WindowsAndMessaging::SetWindowLongPtrW;
use windows::Win32::UI::WindowsAndMessaging::SetWindowPos;
use windows::Win32::UI::WindowsAndMessaging::WINDOWINFO;
use windows::Win32::UI::WindowsAndMessaging::WS_CHILD;
use windows::Win32::UI::WindowsAndMessaging::WS_EX_LAYERED;
use windows::Win32::UI::WindowsAndMessaging::WS_EX_NOREDIRECTIONBITMAP;
use windows::Win32::UI::WindowsAndMessaging::WS_OVERLAPPEDWINDOW;
use windows::Win32::UI::WindowsAndMessaging::WS_POPUP;
use windows::core::BOOL;
use windows::core::w;
use wry::WebView;
use wry::WebViewBuilder;
use wry::WebViewBuilderExtWindows;
use wry::http::Request;
use wry::http::Response;
use wry::http::StatusCode;
use wry::http::header;

const FILL_SCREEN: &str = "Fill Screen";
const FIT_TO_SCREEN: &str = "Fit to Screen";
const CREATE_WORKER_WINDOW_LPARAM: isize = 1;
const CREATE_WORKER_WINDOW_MESSAGE: u32 = 0x052C;
const CREATE_WORKER_WINDOW_WPARAM: usize = 0xD;
const DESKTOP_MESSAGE_TIMEOUT_MS: u32 = 1000;
const PROTOCOL: &str = "paperwall";
const READ_CHUNK_SIZE: u64 = 1024 * 1024;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum BrowserEvent {
    PickVideoFolder,
    SelectVideo { path: String },
    UpdateScalingMode { scaling_mode: String },
}

struct App {
    settings: Settings,
    settings_path: PathBuf,
    wallpaper: Option<Wallpaper>,
}

#[derive(Clone, Deserialize, Serialize)]
struct Settings {
    video_folder: PathBuf,
    scaling_mode: String,
}

struct Wallpaper {
    _window: Window,
    video_path: Rc<RefCell<PathBuf>>,
    version: u64,
    webview: WebView,
}

struct DesktopParent {
    insert_after: HWND,
    layered: bool,
    window: HWND,
}

fn create_picker(
    event_loop: &EventLoopWindowTarget<BrowserEvent>,
    proxy: EventLoopProxy<BrowserEvent>,
    settings: &Settings,
) -> Result<(Window, WebView), Box<dyn Error>> {
    let window = WindowBuilder::new()
        .with_inner_size(LogicalSize::new(800.0, 600.0))
        .with_min_inner_size(LogicalSize::new(800.0, 600.0))
        .with_title("PaperWall")
        .build(event_loop)?;
    let webview = WebViewBuilder::new()
        .with_html(create_picker_html(settings)?)
        .with_ipc_handler(move |request: Request<String>| {
            if let Ok(browser_event) = serde_json::from_str::<BrowserEvent>(request.body()) {
                let _ = proxy.send_event(browser_event);
            }
        })
        .build(&window)?;
    Ok((window, webview))
}

fn create_picker_html(settings: &Settings) -> Result<String, Box<dyn Error>> {
    let html = include_str!("main.html")
        .replace(
            "<link rel=\"stylesheet\" href=\"main.css\">",
            &format!("<style>{}</style>", include_str!("main.css")),
        )
        .replace(
            "<script src=\"main.js\"></script>",
            &format!("<script>{}</script>", include_str!("main.js")),
        )
        .replace(
            "{{video_folder}}",
            &escape_html(&settings.video_folder.display().to_string()),
        )
        .replace(
            "{{scaling_mode_options}}",
            &create_scaling_mode_options(&settings.scaling_mode),
        )
        .replace("{{tiles}}", &create_tiles(&settings.video_folder)?);
    Ok(html)
}

fn create_scaling_mode_options(scaling_mode: &str) -> String {
    [FILL_SCREEN, FIT_TO_SCREEN]
        .iter()
        .map(|option| {
            let selected = if *option == scaling_mode {
                " selected"
            } else {
                ""
            };
            format!("<option{selected}>{}</option>", escape_html(option))
        })
        .collect::<Vec<_>>()
        .join("")
}

fn create_tiles(video_folder: &Path) -> Result<String, Box<dyn Error>> {
    if !video_folder.is_dir() {
        return Ok(String::new());
    }
    let mut video_paths = fs::read_dir(video_folder)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| is_mp4(path))
        .collect::<Vec<_>>();
    video_paths.sort();
    let mut tiles = Vec::new();
    for video_path in video_paths {
        let file_name = video_path
            .file_name()
            .and_then(|file_name| file_name.to_str())
            .unwrap_or("Video");
        let file_url = create_file_url(&video_path)?;
        tiles.push(format!(
            r#"<button
				aria-label="{}"
				aria-pressed="false"
				class="tile"
				data-video-path="{}"
				title="{}"
				type="button"
			>
				<video muted preload="metadata" src="{}"></video>
			</button>"#,
            escape_html(file_name),
            escape_html(&video_path.display().to_string()),
            escape_html(file_name),
            escape_html(&file_url)
        ));
    }
    Ok(tiles.join(""))
}

fn create_file_url(path: &Path) -> Result<String, Box<dyn Error>> {
    url::Url::from_file_path(path)
        .map(|url| url.to_string())
        .map_err(|_| format!("Could not create file URL for {}", path.display()).into())
}

fn create_settings(settings_path: &Path) -> Result<Settings, Box<dyn Error>> {
    if settings_path.is_file() {
        let settings_json = fs::read_to_string(settings_path)?;
        let mut settings = serde_json::from_str::<Settings>(&settings_json)?;
        if !is_scaling_mode(&settings.scaling_mode) {
            settings.scaling_mode = FILL_SCREEN.to_string();
        }
        return Ok(settings);
    }
    Ok(Settings {
        video_folder: default_video_folder()?,
        scaling_mode: FILL_SCREEN.to_string(),
    })
}

fn create_settings_path() -> Result<PathBuf, Box<dyn Error>> {
    let local_app_data = env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is not set")?;
    Ok(PathBuf::from(local_app_data)
        .join("paperwall")
        .join("settings.json"))
}

fn create_wallpaper(
    event_loop: &EventLoopWindowTarget<BrowserEvent>,
    video_path: &Path,
    scaling_mode: &str,
) -> Result<Wallpaper, Box<dyn Error>> {
    let desktop_parent = await_desktop_parent()?;
    create_wallpaper_with_parent(event_loop, video_path, scaling_mode, desktop_parent)
}

fn create_wallpaper_with_parent(
    event_loop: &EventLoopWindowTarget<BrowserEvent>,
    video_path: &Path,
    scaling_mode: &str,
    desktop_parent: DesktopParent,
) -> Result<Wallpaper, Box<dyn Error>> {
    let screen = create_virtual_screen();
    let video_path = Rc::new(RefCell::new(video_path.to_path_buf()));
    let protocol_video_path = Rc::clone(&video_path);
    let protocol_scaling_mode = scaling_mode.to_string();
    let window = WindowBuilder::new()
        .with_decorations(false)
        .with_inner_size(PhysicalSize::new(screen.2 as u32, screen.3 as u32))
        .with_position(PhysicalPosition::new(screen.0, screen.1))
        .with_resizable(false)
        .with_skip_taskbar(true)
        .with_title("PaperWall Wallpaper")
        .with_visible(false)
        .build(event_loop)?;
    let window_handle = HWND(window.hwnd() as _);
    let webview = WebViewBuilder::new()
        .with_autoplay(true)
        .with_default_context_menus(false)
        .with_custom_protocol(
            PROTOCOL.to_string(),
            move |_webview_id, request| match request.uri().path() {
                "/" | "/index.html" => {
                    create_html_response(create_wallpaper_html(&protocol_scaling_mode, 0))
                }
                "/video.mp4" => {
                    match create_video_response(&protocol_video_path.borrow(), &request) {
                        Ok(response) => response,
                        Err(error) => {
                            eprintln!("Serving wallpaper video failed | {error}");
                            create_text_response(
                                StatusCode::INTERNAL_SERVER_ERROR,
                                &error.to_string(),
                            )
                        }
                    }
                }
                _ => create_text_response(StatusCode::NOT_FOUND, "Not found"),
            },
        )
        .with_url(create_wallpaper_page_url())
        .with_ipc_handler(|request: Request<String>| {
            eprintln!("{}", request.body());
        })
        .build(&window)?;
    attach_wallpaper_window(window_handle, &desktop_parent, screen)?;
    eprintln!(
        "Created wallpaper window | hwnd={:?} | parent={:?}",
        window_handle.0, desktop_parent.window.0
    );
    Ok(Wallpaper {
        _window: window,
        video_path,
        version: 0,
        webview,
    })
}

fn create_virtual_screen() -> (i32, i32, i32, i32) {
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

fn create_text_response(status: StatusCode, text: &str) -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Cow::Owned(text.as_bytes().to_vec()))
        .unwrap()
}

fn create_html_response(html: String) -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(Cow::Owned(html.into_bytes()))
        .unwrap()
}

fn create_video_response(
    video_path: &Path,
    request: &Request<Vec<u8>>,
) -> Result<Response<Cow<'static, [u8]>>, Box<dyn Error>> {
    let mut file = File::open(video_path)?;
    let length = read_file_length(&mut file)?;
    let builder = Response::builder()
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_TYPE, "video/mp4");
    if let Some(range_header) = request.headers().get(header::RANGE) {
        let ranges = match HttpRange::parse(range_header.to_str()?, length) {
            Ok(ranges) => ranges,
            Err(_) => return Ok(create_video_range_response(length)),
        };
        let Some(range) = ranges.first() else {
            return Ok(create_video_range_response(length));
        };
        let start = range.start;
        if length == 0 || range.length == 0 || start >= length {
            return Ok(create_video_range_response(length));
        }
        let read_length = range.length.min(READ_CHUNK_SIZE).min(length - start);
        let end = range.start + read_length - 1;
        let bytes = read_video_bytes(&mut file, start, read_length)?;
        return Ok(builder
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_LENGTH, bytes.len())
            .header(
                header::CONTENT_RANGE,
                format!("bytes {start}-{end}/{length}"),
            )
            .body(Cow::Owned(bytes))?);
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(builder
        .header(header::CONTENT_LENGTH, bytes.len())
        .body(Cow::Owned(bytes))?)
}

fn create_video_range_response(length: u64) -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .status(StatusCode::RANGE_NOT_SATISFIABLE)
        .header(header::CONTENT_RANGE, format!("bytes */{length}"))
        .body(Cow::Owned(Vec::new()))
        .unwrap()
}

fn create_video_url(version: u64) -> String {
    format!("video.mp4?v={version}")
}

fn create_wallpaper_page_url() -> String {
    format!("{PROTOCOL}://localhost/index.html")
}

fn read_file_length(file: &mut File) -> Result<u64, Box<dyn Error>> {
    let position = file.stream_position()?;
    let length = file.seek(SeekFrom::End(0))?;
    file.seek(SeekFrom::Start(position))?;
    Ok(length)
}

fn read_video_bytes(file: &mut File, start: u64, length: u64) -> Result<Vec<u8>, Box<dyn Error>> {
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(length).read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn create_wallpaper_html(scaling_mode: &str, version: u64) -> String {
    let object_fit = object_fit(scaling_mode);
    include_str!("wallpaper.html")
        .replace("{{fill_screen}}", FILL_SCREEN)
        .replace("{{object_fit}}", object_fit)
        .replace("{{video_url}}", &escape_html(&create_video_url(version)))
}

fn await_desktop_parent() -> Result<DesktopParent, Box<dyn Error>> {
    let program_manager = unsafe { FindWindowW(w!("Progman"), None)? };
    unsafe {
        SendMessageTimeoutW(
            program_manager,
            CREATE_WORKER_WINDOW_MESSAGE,
            WPARAM(CREATE_WORKER_WINDOW_WPARAM),
            LPARAM(CREATE_WORKER_WINDOW_LPARAM),
            SMTO_NORMAL,
            DESKTOP_MESSAGE_TIMEOUT_MS,
            None,
        );
    }
    let program_manager_style = unsafe { GetWindowLongPtrW(program_manager, GWL_EXSTYLE) };
    if program_manager_style & WS_EX_NOREDIRECTIONBITMAP.0 as isize != 0 {
        let shell_view =
            unsafe { FindWindowExW(Some(program_manager), None, w!("SHELLDLL_DefView"), None)? };
        eprintln!("Using desktop parent | Progman raised desktop");
        return Ok(DesktopParent {
            insert_after: shell_view,
            layered: true,
            window: program_manager,
        });
    }
    if let Some(worker_window) = find_worker_window() {
        eprintln!("Using desktop parent | WorkerW");
        return Ok(DesktopParent {
            insert_after: HWND_BOTTOM,
            layered: false,
            window: worker_window,
        });
    }
    Err("Finding the desktop wallpaper layer failed".into())
}

fn attach_wallpaper_window(
    window_handle: HWND,
    desktop_parent: &DesktopParent,
    screen: (i32, i32, i32, i32),
) -> Result<(), Box<dyn Error>> {
    unsafe {
        SetLastError(NO_ERROR);
        let style = GetWindowLongPtrW(window_handle, GWL_STYLE);
        let style_error = GetLastError();
        if style == 0 && style_error != NO_ERROR {
            return Err(format!(
                "Reading wallpaper window style failed | code={}",
                style_error.0
            )
            .into());
        }
        SetLastError(NO_ERROR);
        let frame_style = WS_OVERLAPPEDWINDOW.0 | WS_POPUP.0;
        SetWindowLongPtrW(
            window_handle,
            GWL_STYLE,
            (style & !(frame_style as isize)) | WS_CHILD.0 as isize,
        );
        let style_error = GetLastError();
        if style_error != NO_ERROR {
            return Err(format!(
                "Updating wallpaper window style failed | code={}",
                style_error.0
            )
            .into());
        }
        if desktop_parent.layered {
            let extended_style = GetWindowLongPtrW(window_handle, GWL_EXSTYLE);
            SetWindowLongPtrW(
                window_handle,
                GWL_EXSTYLE,
                extended_style | WS_EX_LAYERED.0 as isize,
            );
            SetLayeredWindowAttributes(window_handle, COLORREF(0), u8::MAX, LWA_ALPHA)?;
        }
        SetLastError(NO_ERROR);
        let _ = SetParent(window_handle, Some(desktop_parent.window));
        let parent_error = GetLastError();
        if parent_error != NO_ERROR {
            return Err(format!(
                "Attaching wallpaper window failed | code={}",
                parent_error.0
            )
            .into());
        }
        SetWindowPos(
            window_handle,
            Some(desktop_parent.insert_after),
            0,
            0,
            screen.2,
            screen.3,
            SWP_FRAMECHANGED | SWP_NOACTIVATE,
        )?;
        let mut window_info = WINDOWINFO::default();
        window_info.cbSize = mem::size_of_val(&window_info) as u32;
        GetWindowInfo(window_handle, &mut window_info)?;
        let client_width = window_info.rcClient.right - window_info.rcClient.left;
        let client_height = window_info.rcClient.bottom - window_info.rcClient.top;
        let window_width = window_info.rcWindow.right - window_info.rcWindow.left;
        let window_height = window_info.rcWindow.bottom - window_info.rcWindow.top;
        let frame_width = window_width - client_width;
        let frame_height = window_height - client_height;
        SetWindowPos(
            window_handle,
            Some(desktop_parent.insert_after),
            window_info.rcWindow.left - window_info.rcClient.left,
            window_info.rcWindow.top - window_info.rcClient.top,
            screen.2 + frame_width,
            screen.3 + frame_height,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        )?;
    }
    Ok(())
}

fn default_video_folder() -> Result<PathBuf, Box<dyn Error>> {
    let user_profile = env::var_os("USERPROFILE").ok_or("USERPROFILE is not set")?;
    Ok(PathBuf::from(user_profile).join("Videos"))
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn find_worker_window() -> Option<HWND> {
    unsafe extern "system" fn enum_window(window: HWND, worker_window: LPARAM) -> BOOL {
        let shell_view = unsafe { FindWindowExW(Some(window), None, w!("SHELLDLL_DefView"), None) };
        if shell_view.is_ok()
            && let Ok(worker) = unsafe { FindWindowExW(None, Some(window), w!("WorkerW"), None) }
        {
            unsafe {
                *(worker_window.0 as *mut HWND) = worker;
            }
            return BOOL(0);
        }
        BOOL(1)
    }
    let mut worker_window = HWND::default();
    unsafe {
        let worker_window_address = &mut worker_window as *mut HWND as isize;
        let _ = EnumWindows(Some(enum_window), LPARAM(worker_window_address));
    }
    if worker_window.is_invalid() {
        return None;
    }
    Some(worker_window)
}

fn is_mp4(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("mp4"))
}

fn is_scaling_mode(scaling_mode: &str) -> bool {
    scaling_mode == FILL_SCREEN || scaling_mode == FIT_TO_SCREEN
}

fn object_fit(scaling_mode: &str) -> &str {
    if scaling_mode == FILL_SCREEN {
        return "cover";
    }
    "contain"
}

fn save_settings(settings_path: &Path, settings: &Settings) -> Result<(), Box<dyn Error>> {
    if let Some(settings_folder) = settings_path.parent() {
        fs::create_dir_all(settings_folder)?;
    }
    fs::write(settings_path, serde_json::to_string_pretty(settings)?)?;
    Ok(())
}

fn set_picker_folder(webview: &WebView, settings: &Settings) -> Result<(), Box<dyn Error>> {
    let video_folder = settings.video_folder.display().to_string();
    let tiles = create_tiles(&settings.video_folder)?;
    let script = format!(
        "window.paperwall.setVideoFolder({}, {});",
        serde_json::to_string(&video_folder)?,
        serde_json::to_string(&tiles)?
    );
    webview.evaluate_script(&script)?;
    Ok(())
}

fn set_wallpaper_scaling_mode(webview: &WebView, scaling_mode: &str) -> Result<(), Box<dyn Error>> {
    let script = format!(
        "window.paperwall.setScalingMode({});",
        serde_json::to_string(scaling_mode)?
    );
    webview.evaluate_script(&script)?;
    Ok(())
}

fn set_wallpaper_video(
    wallpaper: &mut Wallpaper,
    video_path: PathBuf,
) -> Result<(), Box<dyn Error>> {
    *wallpaper.video_path.borrow_mut() = video_path;
    wallpaper.version += 1;
    let script = format!(
        "window.paperwall.setVideoSource({});",
        serde_json::to_string(&create_video_url(wallpaper.version))?
    );
    wallpaper.webview.evaluate_script(&script)?;
    Ok(())
}

fn update_scaling_mode(app: &mut App, scaling_mode: String) -> Result<(), Box<dyn Error>> {
    if !is_scaling_mode(&scaling_mode) {
        return Ok(());
    }
    app.settings.scaling_mode = scaling_mode;
    save_settings(&app.settings_path, &app.settings)?;
    if let Some(wallpaper) = &app.wallpaper {
        set_wallpaper_scaling_mode(&wallpaper.webview, &app.settings.scaling_mode)?;
    }
    Ok(())
}

fn update_video_folder(
    app: &mut App,
    picker_webview: &WebView,
    video_folder: PathBuf,
) -> Result<(), Box<dyn Error>> {
    app.settings.video_folder = video_folder;
    save_settings(&app.settings_path, &app.settings)?;
    set_picker_folder(picker_webview, &app.settings)?;
    Ok(())
}

fn update_wallpaper(
    app: &mut App,
    event_loop: &EventLoopWindowTarget<BrowserEvent>,
    video_path: PathBuf,
) -> Result<(), Box<dyn Error>> {
    if !is_mp4(&video_path) || !video_path.is_file() {
        return Ok(());
    }
    if let Some(wallpaper) = &mut app.wallpaper {
        set_wallpaper_video(wallpaper, video_path)?;
        return Ok(());
    }
    app.wallpaper = Some(create_wallpaper(
        event_loop,
        &video_path,
        &app.settings.scaling_mode,
    )?);
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let settings_path = create_settings_path()?;
    let settings = create_settings(&settings_path)?;
    save_settings(&settings_path, &settings)?;
    let event_loop = EventLoopBuilder::<BrowserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let (_picker_window, picker_webview) = create_picker(&event_loop, proxy, &settings)?;
    let mut app = App {
        settings,
        settings_path,
        wallpaper: None,
    };
    event_loop.run(move |event, event_loop, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => *control_flow = ControlFlow::Exit,
            Event::UserEvent(BrowserEvent::PickVideoFolder) => {
                if let Some(video_folder) = rfd::FileDialog::new()
                    .set_directory(&app.settings.video_folder)
                    .pick_folder()
                {
                    match update_video_folder(&mut app, &picker_webview, video_folder) {
                        Ok(()) => {}
                        Err(error) => eprintln!("Updating video folder failed | {error}"),
                    }
                }
            }
            Event::UserEvent(BrowserEvent::SelectVideo { path }) => {
                if let Err(error) = update_wallpaper(&mut app, event_loop, PathBuf::from(path)) {
                    eprintln!("Updating wallpaper failed | {error}");
                }
            }
            Event::UserEvent(BrowserEvent::UpdateScalingMode { scaling_mode }) => {
                if let Err(error) = update_scaling_mode(&mut app, scaling_mode) {
                    eprintln!("Updating scaling mode failed | {error}");
                }
            }
            _ => {}
        }
    });
}
