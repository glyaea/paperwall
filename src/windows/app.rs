use super::*;
use http_range::HttpRange;
use std::borrow::Cow;
use std::cell::RefCell;
use std::env;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::mem;
use std::rc::Rc;
use tao::dpi::{PhysicalPosition, PhysicalSize};
use tao::event_loop::{EventLoopProxy, EventLoopWindowTarget};
use tao::platform::windows::{WindowBuilderExtWindows, WindowExtWindows};
use windows::Win32::Foundation::{
	COLORREF, GetLastError, HWND, LPARAM, NO_ERROR, SetLastError, WPARAM
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{FOLDERID_Videos, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
use windows::Win32::UI::WindowsAndMessaging::{
	EnumWindows, FindWindowExW, FindWindowW, GWL_EXSTYLE, GWL_STYLE, GetSystemMetrics,
	GetWindowInfo, GetWindowLongPtrW, HWND_BOTTOM, LWA_ALPHA, SM_CXVIRTUALSCREEN,
	SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SMTO_NORMAL,
	SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_SHOWWINDOW, SendMessageTimeoutW,
	SetLayeredWindowAttributes, SetParent, SetWindowLongPtrW, SetWindowPos, WINDOWINFO, WS_CHILD,
	WS_EX_LAYERED, WS_EX_NOREDIRECTIONBITMAP, WS_OVERLAPPEDWINDOW, WS_POPUP
};
use windows::core::{BOOL, w};
use wry::http::{header, Request, Response, StatusCode};
use wry::{WebView, WebViewBuilder, WebViewBuilderExtWindows};

const APP_JS: &str = include_str!("app.js");
const CREATE_WORKER_WINDOW_LPARAM: isize = 1;
const CREATE_WORKER_WINDOW_MESSAGE: u32 = 0x052C;
const CREATE_WORKER_WINDOW_WPARAM: usize = 0xD;
const DESKTOP_MESSAGE_TIMEOUT_MS: u32 = 1000;
const PROTOCOL: &str = "paperwall";
const READ_CHUNK_SIZE: u64 = 1024 * 1024;
const WALLPAPER_CSS: &str = include_str!("wallpaper.css");

pub struct Picker {
	webview: WebView,
	window: Window
}

pub struct Wallpaper {
	_window: Window,
	video_path: Rc<RefCell<PathBuf>>,
	version: u64,
	webview: WebView
}

struct DesktopParent {
	insert_after: HWND,
	layered: bool,
	window: HWND
}

fn attach_wallpaper_window(
	window_handle: HWND,
	desktop_parent: &DesktopParent,
	screen: (i32, i32, i32, i32)
) -> Result<(), Box<dyn Error>> {
	unsafe {
		SetLastError(NO_ERROR);
		let style = GetWindowLongPtrW(window_handle, GWL_STYLE);
		let style_error = GetLastError();
		if style == 0 && style_error != NO_ERROR {
			return Err(format!(
				"Reading wallpaper window style failed | code={}",
				style_error.0
			).into());
		}
		SetLastError(NO_ERROR);
		let frame_style = WS_OVERLAPPEDWINDOW.0 | WS_POPUP.0;
		SetWindowLongPtrW(
			window_handle,
			GWL_STYLE,
			(style & !(frame_style as isize)) | WS_CHILD.0 as isize
		);
		let style_error = GetLastError();
		if style_error != NO_ERROR {
			return Err(format!(
				"Updating wallpaper window style failed | code={}",
				style_error.0
			).into());
		}
		if desktop_parent.layered {
			let extended_style = GetWindowLongPtrW(window_handle, GWL_EXSTYLE);
			SetWindowLongPtrW(
				window_handle,
				GWL_EXSTYLE,
				extended_style | WS_EX_LAYERED.0 as isize
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
			).into());
		}
		SetWindowPos(
			window_handle,
			Some(desktop_parent.insert_after),
			0,
			0,
			screen.2,
			screen.3,
			SWP_FRAMECHANGED | SWP_NOACTIVATE
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
			SWP_NOACTIVATE | SWP_SHOWWINDOW
		)?;
	}
	Ok(())
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
			None
		);
	}
	let program_manager_style = unsafe { GetWindowLongPtrW(program_manager, GWL_EXSTYLE) };
	if program_manager_style & WS_EX_NOREDIRECTIONBITMAP.0 as isize != 0 {
		let shell_view = unsafe {
			FindWindowExW(Some(program_manager), None, w!("SHELLDLL_DefView"), None)?
		};
		eprintln!("Using desktop parent | Progman raised desktop");
		return Ok(DesktopParent {
			insert_after: shell_view,
			layered: true,
			window: program_manager
		});
	}
	if let Some(worker_window) = find_worker_window() {
		eprintln!("Using desktop parent | WorkerW");
		return Ok(DesktopParent {
			insert_after: HWND_BOTTOM,
			layered: false,
			window: worker_window
		});
	}
	Err("Finding the desktop wallpaper layer failed".into())
}

fn create_file_url(path: &Path) -> Result<String, Box<dyn Error>> {
	url::Url::from_file_path(path)
		.map(|url| url.to_string())
		.map_err(|_| format!("Could not create file URL for {}", path.display()).into())
}

fn create_html_response(html: String) -> Response<Cow<'static, [u8]>> {
	Response::builder()
		.header(header::CONTENT_TYPE, "text/html; charset=utf-8")
		.body(Cow::Owned(html.into_bytes()))
		.unwrap()
}

fn create_text_response(status: StatusCode, text: &str) -> Response<Cow<'static, [u8]>> {
	Response::builder()
		.status(status)
		.header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
		.body(Cow::Owned(text.as_bytes().to_vec()))
		.unwrap()
}

fn create_video_range_response(length: u64) -> Response<Cow<'static, [u8]>> {
	Response::builder()
		.status(StatusCode::RANGE_NOT_SATISFIABLE)
		.header(header::CONTENT_RANGE, format!("bytes */{length}"))
		.body(Cow::Owned(Vec::new()))
		.unwrap()
}

fn create_video_response(
	video_path: &Path,
	request: &Request<Vec<u8>>
) -> Result<Response<Cow<'static, [u8]>>, Box<dyn Error>> {
	let mut file = File::open(video_path)?;
	let length = read_file_length(&mut file)?;
	let builder = Response::builder()
		.header(header::ACCEPT_RANGES, "bytes")
		.header(header::CONTENT_TYPE, "video/mp4");
	if let Some(range_header) = request.headers().get(header::RANGE) {
		let ranges = match HttpRange::parse(range_header.to_str()?, length) {
			Ok(ranges) => ranges,
			Err(_) => return Ok(create_video_range_response(length))
		};
		let Some(range) = ranges.first() else {
			return Ok(create_video_range_response(length));
		};
		let start = range.start;
		if length == 0 || range.length == 0 || start >= length {
			return Ok(create_video_range_response(length));
		}
		let read_length = range.length.min(READ_CHUNK_SIZE).min(length - start);
		let end = start + read_length - 1;
		let bytes = read_video_bytes(&mut file, start, read_length)?;
		return Ok(builder
			.status(StatusCode::PARTIAL_CONTENT)
			.header(header::CONTENT_LENGTH, bytes.len())
			.header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{length}"))
			.body(Cow::Owned(bytes))?);
	}
	let mut bytes = Vec::new();
	file.read_to_end(&mut bytes)?;
	Ok(builder
		.header(header::CONTENT_LENGTH, bytes.len())
		.body(Cow::Owned(bytes))?)
}

fn create_video_url(version: u64) -> String {
	format!("video.mp4?v={version}")
}

fn create_virtual_screen() -> (i32, i32, i32, i32) {
	unsafe {
		(
			GetSystemMetrics(SM_XVIRTUALSCREEN),
			GetSystemMetrics(SM_YVIRTUALSCREEN),
			GetSystemMetrics(SM_CXVIRTUALSCREEN),
			GetSystemMetrics(SM_CYVIRTUALSCREEN)
		)
	}
}

fn create_wallpaper_html(scaling_mode: ScalingMode, version: u64) -> String {
	include_str!("wallpaper.html")
		.replace("{{fill_screen}}", ScalingMode::FillScreen.label())
		.replace("{{object_fit}}", object_fit(scaling_mode))
		.replace("{{video_url}}", &escape_html(&create_video_url(version)))
}

fn create_wallpaper_page_url() -> String {
	format!("{PROTOCOL}://localhost/index.html")
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

fn object_fit(scaling_mode: ScalingMode) -> &'static str {
	if scaling_mode == ScalingMode::FillScreen {
		return "cover";
	}
	"contain"
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

fn set_wallpaper_scaling_mode(
	webview: &WebView,
	scaling_mode: ScalingMode
) -> Result<(), Box<dyn Error>> {
	let scaling_mode = serde_json::to_string(scaling_mode.label())?;
	webview.evaluate_script(&format!(
		"window.paperwall.setScalingMode({scaling_mode})"
	))?;
	Ok(())
}

fn set_wallpaper_video(wallpaper: &mut Wallpaper, video_path: &Path) -> Result<(), Box<dyn Error>> {
	*wallpaper.video_path.borrow_mut() = video_path.to_path_buf();
	wallpaper.version += 1;
	let source = serde_json::to_string(&create_video_url(wallpaper.version))?;
	wallpaper.webview.evaluate_script(&format!(
		"window.paperwall.setVideoSource({source})"
	))?;
	Ok(())
}

pub fn create_thumbnail(video: &Video, _index: usize) -> Result<String, Box<dyn Error>> {
	Ok(format!(
		"<video muted preload=\"metadata\" src=\"{}\"></video>",
		escape_html(&create_file_url(&video.path)?)
	))
}

pub fn read_default_video_folder() -> Result<PathBuf, Box<dyn Error>> {
	let video_folder = unsafe { SHGetKnownFolderPath(&FOLDERID_Videos, KF_FLAG_DEFAULT, None)? };
	let path = unsafe { video_folder.to_string() };
	unsafe {
		CoTaskMemFree(Some(video_folder.as_ptr().cast()));
	}
	Ok(PathBuf::from(path?))
}

pub fn read_settings_path() -> Result<PathBuf, Box<dyn Error>> {
	let local_app_data = env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is not set")?;
	Ok(PathBuf::from(local_app_data).join("paperwall").join("settings.json"))
}

impl Picker {
	pub fn create(
		window: Window,
		proxy: EventLoopProxy<UserEvent>,
		html: String,
		_videos: Arc<Mutex<Vec<Video>>>
	) -> Result<Self, Box<dyn Error>> {
		let html = html
			.replace(
				"<link rel=\"stylesheet\" href=\"main.css\">",
				&format!("<style>{MAIN_CSS}</style>")
			)
			.replace(
				"<script src=\"app.js\"></script>",
				&format!("<script>{APP_JS}</script>")
			)
			.replace(
				"<script src=\"main.js\"></script>",
				&format!("<script>{MAIN_JS}</script>")
			);
		let webview = WebViewBuilder::new()
			.with_html(html)
			.with_ipc_handler(move |request: Request<String>| {
				if let Ok(event) = serde_json::from_str(request.body()) {
					let _ = proxy.send_event(event);
				}
			})
			.build(&window)?;
		Ok(Self { webview, window })
	}

	pub fn set_videos(
		&self,
		_html: String,
		video_folder: &Path,
		tiles: &str
	) -> Result<(), Box<dyn Error>> {
		let video_folder = serde_json::to_string(&video_folder.display().to_string())?;
		let tiles = serde_json::to_string(tiles)?;
		self.webview.evaluate_script(&format!(
			"window.paperwall.setVideos({video_folder}, {tiles})"
		))?;
		Ok(())
	}

	pub fn window_id(&self) -> WindowId {
		self.window.id()
	}
}

impl Wallpaper {
	pub fn create(
		event_loop: &EventLoopWindowTarget<UserEvent>,
		video_path: &Path,
		scaling_mode: ScalingMode
	) -> Result<Self, Box<dyn Error>> {
		let desktop_parent = await_desktop_parent()?;
		let screen = create_virtual_screen();
		let video_path = Rc::new(RefCell::new(video_path.to_path_buf()));
		let protocol_video_path = Rc::clone(&video_path);
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
			.with_custom_protocol(PROTOCOL.to_string(), move |_, request| {
				match request.uri().path() {
					"/" | "/index.html" => {
						create_html_response(create_wallpaper_html(scaling_mode, 0))
					}
					"/video.mp4" => {
						match create_video_response(&protocol_video_path.borrow(), &request) {
							Ok(response) => response,
							Err(error) => {
								eprintln!("Serving wallpaper video failed | {error}");
								create_text_response(
									StatusCode::INTERNAL_SERVER_ERROR,
									&error.to_string()
								)
							}
						}
					}
					"/wallpaper.css" => Response::builder()
						.header(header::CONTENT_TYPE, "text/css")
						.body(Cow::Borrowed(WALLPAPER_CSS.as_bytes()))
						.unwrap(),
					_ => create_text_response(StatusCode::NOT_FOUND, "Not found")
				}
			})
			.with_url(create_wallpaper_page_url())
			.with_ipc_handler(|request: Request<String>| eprintln!("{}", request.body()))
			.build(&window)?;
		attach_wallpaper_window(window_handle, &desktop_parent, screen)?;
		eprintln!(
			"Created wallpaper window | hwnd={:?} | parent={:?}",
			window_handle.0,
			desktop_parent.window.0
		);
		Ok(Self { _window: window, video_path, version: 0, webview })
	}

	pub fn set_scaling_mode(
		&mut self,
		scaling_mode: ScalingMode
	) -> Result<(), Box<dyn Error>> {
		set_wallpaper_scaling_mode(&self.webview, scaling_mode)
	}

	pub fn set_video(
		&mut self,
		video_path: &Path,
		_scaling_mode: ScalingMode
	) -> Result<(), Box<dyn Error>> {
		set_wallpaper_video(self, video_path)
	}
}
