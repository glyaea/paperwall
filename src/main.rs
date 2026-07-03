#[cfg(not(any(target_os = "macos", target_os = "windows")))]
compile_error!("PaperWall currently supports macOS and Windows.");

use rfd::FileDialog;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashMap;
use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tao::window::{Window, WindowBuilder};
use wry::http::{header::CONTENT_TYPE, Request, Response};
use wry::{WebView, WebViewBuilder};

const MAIN_CSS: &str = include_str!("main.css");
const MAIN_HTML: &str = include_str!("main.html");
const MAIN_JS: &str = include_str!("main.js");
const PICKER_MIN_HEIGHT: f64 = 600.0;
const PICKER_MIN_WIDTH: f64 = 800.0;
const WALLPAPER_EVENT_INTERVAL: Duration = Duration::from_secs(1);

struct Video {
	name: String,
	path: PathBuf
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq, Serialize)]
enum ScalingMode {
	#[serde(rename = "Fill Screen")]
	FillScreen,
	#[serde(rename = "Fit to Screen")]
	FitToScreen
}

#[derive(Deserialize, Serialize)]
struct Settings {
	video_folder: PathBuf,
	scaling_mode: ScalingMode
}

enum UserEvent {
	ChooseVideoFolder,
	SetScalingMode(ScalingMode),
	SelectVideo(usize)
}

fn run() -> Result<(), Box<dyn Error>> {
	let settings_path = platform::read_settings_path()?;
	let (mut settings, videos, should_write_settings) = if let Some(settings) =
		read_settings(&settings_path)?
	{
		let videos = read_videos(&settings.video_folder)?;
		(settings, videos, false)
	} else {
		let (settings, videos) = read_default_settings_and_videos()?;
		(settings, videos, true)
	};
	if let Err(error) = platform::prepare_preview_cache(&videos) {
		eprintln!("Reporting cache error | {error}");
	}
	let picker_html = Arc::new(Mutex::new(create_picker_html(
		&settings.video_folder,
		&videos,
		settings.scaling_mode
	)));
	let preview_cache = Arc::new(Mutex::new(HashMap::new()));
	let videos = Arc::new(Mutex::new(videos));
	let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
	let proxy = event_loop.create_proxy();
	let picker_window = create_picker_window(&event_loop)?;
	let wallpaper_window = platform::create_wallpaper_window(&event_loop)?;
	platform::configure_wallpaper_window(&wallpaper_window)?;
	let picker_webview = create_picker_webview(
		&picker_window,
		proxy,
		Arc::clone(&picker_html),
		Arc::clone(&videos),
		Arc::clone(&preview_cache)
	)?;
	let mut picker_webview = Some(picker_webview);
	let mut selected_video = None;
	let mut scaling_mode = settings.scaling_mode;
	let mut wallpaper_player = platform::create_wallpaper_player(&wallpaper_window)?;
	wallpaper_player.set_scaling_mode(scaling_mode);
	if should_write_settings && let Err(error) = write_settings(&settings_path, &settings) {
		eprintln!("Reporting settings error | {error}");
	}

	event_loop.run(move |event, _, control_flow| {
		let mut should_exit = false;
		match event {
			Event::WindowEvent { event: WindowEvent::CloseRequested, window_id, .. } => {
				if window_id == picker_window.id() {
					let _ = picker_webview.take();
					should_exit = true;
				}
			}
			Event::UserEvent(UserEvent::ChooseVideoFolder) => {
				if let Some(video_folder) = FileDialog::new()
					.set_directory(&settings.video_folder)
					.pick_folder()
				{
					match read_videos(&video_folder) {
						Ok(next_videos) => {
							if let Err(error) = platform::prepare_preview_cache(&next_videos) {
								eprintln!("Reporting cache error | {error}");
							}
							let next_html = create_picker_html(
								&video_folder,
								&next_videos,
								settings.scaling_mode
							);
							let update_script = create_update_videos_script(
								&video_folder,
								&next_videos
							);
							settings.video_folder = video_folder;
							selected_video = None;
							preview_cache.lock().unwrap().clear();
							*videos.lock().unwrap() = next_videos;
							*picker_html.lock().unwrap() = next_html;
							if let Err(error) = write_settings(&settings_path, &settings) {
								eprintln!("Reporting settings error | {error}");
							}
							if let (Some(webview), Ok(script)) =
								(picker_webview.as_ref(), update_script)
							{
								if let Err(error) = webview.evaluate_script(&script) {
									eprintln!("Reporting picker update error | {error}");
								}
							}
						}
						Err(error) => eprintln!("Reporting video folder error | {error}")
					}
				}
			}
			Event::UserEvent(UserEvent::SetScalingMode(next_scaling_mode)) => {
				scaling_mode = next_scaling_mode;
				settings.scaling_mode = scaling_mode;
				if let Some((index, _)) = selected_video {
					selected_video = Some((index, scaling_mode));
				}
				wallpaper_player.set_scaling_mode(scaling_mode);
				*picker_html.lock().unwrap() = create_picker_html(
					&settings.video_folder,
					&videos.lock().unwrap(),
					settings.scaling_mode
				);
				if let Err(error) = write_settings(&settings_path, &settings) {
					eprintln!("Reporting settings error | {error}");
				}
			}
			Event::UserEvent(UserEvent::SelectVideo(index)) => {
				if selected_video != Some((index, scaling_mode)) {
					let path = videos.lock().unwrap().get(index).map(|video| video.path.clone());
					if let Some(path) = path {
						if let Err(error) = wallpaper_player.play(&path, scaling_mode) {
							eprintln!("Reporting playback error | {error}");
						} else {
							selected_video = Some((index, scaling_mode));
							platform::show_wallpaper_window(&wallpaper_window);
						}
					}
				}
			}
			_ => {}
		}
		if should_exit {
			*control_flow = ControlFlow::Exit;
			return;
		}
		if wallpaper_player.needs_poll() {
			if let Err(error) = wallpaper_player.poll() {
				eprintln!("Reporting playback event error | {error}");
			}
			*control_flow = ControlFlow::WaitUntil(Instant::now() + WALLPAPER_EVENT_INTERVAL);
		} else {
			*control_flow = ControlFlow::Wait;
		}
	});
}

fn create_picker_webview(
	window: &Window,
	proxy: EventLoopProxy<UserEvent>,
	html: Arc<Mutex<String>>,
	videos: Arc<Mutex<Vec<Video>>>,
	preview_cache: Arc<Mutex<HashMap<usize, Vec<u8>>>>
) -> wry::Result<WebView> {
	let protocol_html = Arc::clone(&html);
	let protocol_videos = Arc::clone(&videos);
	let protocol_preview_cache = Arc::clone(&preview_cache);
	let handler = move |request: Request<String>| {
		let body = request.body();
		if body == "video-folder:choose" {
			let _ = proxy.send_event(UserEvent::ChooseVideoFolder);
		} else if let Some(index) = body.strip_prefix("select:") {
			if let Ok(index) = index.parse() {
				let _ = proxy.send_event(UserEvent::SelectVideo(index));
			}
		} else if let Some(scaling_mode) = body.strip_prefix("scaling-mode:") {
			if let Some(scaling_mode) = ScalingMode::from_label(scaling_mode) {
				let _ = proxy.send_event(UserEvent::SetScalingMode(scaling_mode));
			}
		}
	};
	WebViewBuilder::new()
		.with_custom_protocol("paperwall".into(), move |_, request| {
			create_asset_response(
				request,
				&protocol_html,
				&protocol_videos,
				&protocol_preview_cache
			)
		})
		.with_ipc_handler(handler)
		.with_url("paperwall://localhost/main.html")
		.build(window)
}

fn create_picker_window(
	event_loop: &EventLoopWindowTarget<UserEvent>
) -> Result<Window, tao::error::OsError> {
	WindowBuilder::new()
		.with_min_inner_size(LogicalSize::new(PICKER_MIN_WIDTH, PICKER_MIN_HEIGHT))
		.with_title("PaperWall")
		.build(event_loop)
}

fn create_asset_response(
	request: Request<Vec<u8>>,
	html: &Arc<Mutex<String>>,
	videos: &Arc<Mutex<Vec<Video>>>,
	preview_cache: &Arc<Mutex<HashMap<usize, Vec<u8>>>>
) -> Response<Cow<'static, [u8]>> {
	if request.uri().path().starts_with("/preview/") {
		if let Some(response) =
			platform::create_preview_response(&request, &videos.lock().unwrap(), preview_cache)
		{
			return response;
		}
	}
	match request.uri().path() {
		"/" | "/main.html" => {
			let html = html.lock().unwrap();
			create_response("text/html", Cow::Owned(html.as_bytes().to_vec()))
		}
		"/main.css" => create_response("text/css", Cow::Borrowed(MAIN_CSS.as_bytes())),
		"/main.js" => create_response("text/javascript", Cow::Borrowed(MAIN_JS.as_bytes())),
		_ => Response::builder()
			.header(CONTENT_TYPE, "text/plain")
			.status(404)
			.body(Cow::Borrowed(&b"Not found"[..]))
			.unwrap()
	}
}

fn create_picker_html(
	video_folder: &Path,
	videos: &[Video],
	scaling_mode: ScalingMode
) -> String {
	MAIN_HTML
		.replace("{{video_folder}}", &escape_html(&video_folder.display().to_string()))
		.replace("{{scaling_mode_options}}", &create_scaling_mode_options(scaling_mode))
		.replace("{{tiles}}", &create_tiles(videos))
}

fn create_scaling_mode_options(scaling_mode: ScalingMode) -> String {
	let mut scaling_mode_options = String::new();
	for option in [ScalingMode::FillScreen, ScalingMode::FitToScreen] {
		if option == scaling_mode {
			let _ = write!(scaling_mode_options, "<option selected>{}</option>", option.label());
		} else {
			let _ = write!(scaling_mode_options, "<option>{}</option>", option.label());
		}
	}
	scaling_mode_options
}

fn create_tiles(videos: &[Video]) -> String {
	let mut tiles = String::new();
	for (index, video) in videos.iter().enumerate() {
		let _ = write!(
			tiles,
			concat!(
				"<button aria-pressed=\"false\" class=\"tile\" name=\"video\" title=\"{}\" ",
				"value=\"{}\"><img alt=\"\" src=\"preview/{}.jpg\"></button>"
			),
			escape_html(&video.name),
			index,
			index
		);
	}
	tiles
}

fn create_update_videos_script(
	video_folder: &Path,
	videos: &[Video]
) -> Result<String, Box<dyn Error>> {
	let video_folder = serde_json::to_string(&video_folder.display().to_string())?;
	let tiles = serde_json::to_string(&create_tiles(videos))?;
	Ok(format!("window.updateVideos({video_folder}, {tiles})"))
}

fn create_response(
	content_type: &str,
	body: Cow<'static, [u8]>
) -> Response<Cow<'static, [u8]>> {
	Response::builder()
		.header(CONTENT_TYPE, content_type)
		.body(body)
		.unwrap()
}

fn escape_html(text: &str) -> String {
	let mut escaped_text = String::with_capacity(text.len());
	for character in text.chars() {
		match character {
			'&' => escaped_text.push_str("&amp;"),
			'<' => escaped_text.push_str("&lt;"),
			'>' => escaped_text.push_str("&gt;"),
			'"' => escaped_text.push_str("&quot;"),
			_ => escaped_text.push(character)
		}
	}
	escaped_text
}

fn is_video_path(path: &Path) -> bool {
	let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
		return false;
	};
	["avi", "m4v", "mkv", "mov", "mp4", "webm"]
		.iter()
		.any(|video_extension| extension.eq_ignore_ascii_case(video_extension))
}

fn read_default_settings_and_videos() -> Result<(Settings, Vec<Video>), Box<dyn Error>> {
	let video_folder = platform::read_default_video_folder()?;
	let scaling_mode = ScalingMode::FillScreen;
	let videos = read_videos(&video_folder)?;
	Ok((Settings { video_folder, scaling_mode }, videos))
}

fn read_settings(path: &Path) -> Result<Option<Settings>, Box<dyn Error>> {
	match fs::read_to_string(path) {
		Ok(settings) => Ok(Some(serde_json::from_str(&settings)?)),
		Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
		Err(error) => Err(error.into())
	}
}

fn read_videos(video_folder: &Path) -> Result<Vec<Video>, Box<dyn Error>> {
	let mut videos = Vec::new();
	let entries = match fs::read_dir(video_folder) {
		Ok(entries) => entries,
		Err(error) if error.kind() == ErrorKind::NotFound => return Ok(videos),
		Err(error) => return Err(error.into())
	};
	for entry in entries {
		let entry = entry?;
		let path = entry.path();
		if path.is_file() && is_video_path(&path) {
			let name = path.file_name().unwrap().to_string_lossy().to_string();
			videos.push(Video { name, path });
		}
	}
	videos.sort_by(|left, right| left.name.cmp(&right.name));
	Ok(videos)
}

fn write_settings(path: &Path, settings: &Settings) -> Result<(), Box<dyn Error>> {
	fs::create_dir_all(path.parent().ok_or("Settings path has no parent")?)?;
	fs::write(path, format!("{}\n", serde_json::to_string_pretty(settings)?))?;
	Ok(())
}

impl ScalingMode {
	fn from_label(label: &str) -> Option<Self> {
		match label {
			"Fill Screen" => Some(Self::FillScreen),
			"Fit to Screen" => Some(Self::FitToScreen),
			_ => None
		}
	}

	fn label(self) -> &'static str {
		match self {
			Self::FillScreen => "Fill Screen",
			Self::FitToScreen => "Fit to Screen"
		}
	}
}

#[cfg(target_os = "macos")]
mod platform {
	use super::*;
	use objc2::encode::{Encode, Encoding, RefEncode};
	use objc2::rc::Retained;
	use objc2::runtime::{AnyObject, Bool};
	use objc2::{class, msg_send, AnyThread};
	use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSBitmapImageRepPropertyKey};
	use objc2_core_graphics::CGImage;
	use objc2_foundation::{
		NSDictionary, NSRect, NSSearchPathDirectory, NSSearchPathDomainMask,
		NSSearchPathForDirectoriesInDomains, NSString, NSURL
	};
	use std::env;
	use std::ptr;
	use tao::dpi::PhysicalSize;
	use tao::platform::macos::WindowExtMacOS;

	const COLLECTION_CAN_JOIN_ALL_SPACES: usize = 1 << 0;
	const COLLECTION_IGNORES_CYCLE: usize = 1 << 6;
	const COLLECTION_STATIONARY: usize = 1 << 4;
	const DESKTOP_WINDOW_LEVEL_KEY: i32 = 2;

	#[link(name = "CoreGraphics", kind = "framework")]
	unsafe extern "C" {
		fn CGImageRelease(image: *mut CGImage);
		fn CGWindowLevelForKey(key: i32) -> i32;
	}

	#[link(name = "CoreMedia", kind = "framework")]
	unsafe extern "C" {
		fn CMTimeGetSeconds(time: CMTime) -> f64;
		fn CMTimeMake(value: i64, timescale: i32) -> CMTime;
	}

	#[link(name = "AVFoundation", kind = "framework")]
	unsafe extern "C" {
		#[link_name = "AVLayerVideoGravityResizeAspect"]
		static AV_LAYER_VIDEO_GRAVITY_RESIZE_ASPECT: *const AnyObject;

		#[link_name = "AVLayerVideoGravityResizeAspectFill"]
		static AV_LAYER_VIDEO_GRAVITY_RESIZE_ASPECT_FILL: *const AnyObject;

		#[link_name = "AVMediaTypeVideo"]
		static AV_MEDIA_TYPE_VIDEO: *const AnyObject;
	}

	#[link(name = "QuartzCore", kind = "framework")]
	unsafe extern "C" {}

	pub struct WallpaperPlayer {
		layer: Retained<AnyObject>,
		looper: Option<Retained<AnyObject>>,
		player: Option<Retained<AnyObject>>,
		view: *mut AnyObject
	}

	#[derive(Clone, Copy)]
	#[repr(C)]
	struct CMTime {
		value: i64,
		timescale: i32,
		flags: u32,
		epoch: i64
	}

	unsafe impl Encode for CMTime {
		const ENCODING: Encoding = Encoding::Struct(
			"CMTime",
			&[i64::ENCODING, i32::ENCODING, u32::ENCODING, i64::ENCODING]
		);
	}

	unsafe impl RefEncode for CMTime {
		const ENCODING_REF: Encoding = Encoding::Pointer(&Self::ENCODING);
	}

	pub fn configure_wallpaper_window(window: &Window) -> Result<(), Box<dyn Error>> {
		window.set_ignore_cursor_events(true)?;
		unsafe {
			let ns_window = window.ns_window() as *mut AnyObject;
			let level = CGWindowLevelForKey(DESKTOP_WINDOW_LEVEL_KEY);
			let collection_behavior = COLLECTION_CAN_JOIN_ALL_SPACES
				| COLLECTION_IGNORES_CYCLE
				| COLLECTION_STATIONARY;
			let _: () = msg_send![ns_window, setCollectionBehavior: collection_behavior];
			let _: () = msg_send![ns_window, setHasShadow: Bool::NO];
			let _: () = msg_send![ns_window, setIgnoresMouseEvents: Bool::YES];
			let _: () = msg_send![ns_window, setLevel: level as isize];
			let _: () = msg_send![ns_window, setOpaque: Bool::YES];
		}
		Ok(())
	}

	pub fn create_preview_response(
		request: &Request<Vec<u8>>,
		videos: &[Video],
		preview_cache: &Arc<Mutex<HashMap<usize, Vec<u8>>>>
	) -> Option<Response<Cow<'static, [u8]>>> {
		let index = request
			.uri()
			.path()
			.strip_prefix("/preview/")
			.and_then(|name| name.strip_suffix(".jpg"))
			.and_then(|name| name.parse::<usize>().ok())?;
		if let Some(preview) = preview_cache.lock().unwrap().get(&index).cloned() {
			return Some(create_response("image/jpeg", Cow::Owned(preview)));
		}
		let video = videos.get(index)?;
		if let Ok(preview) = create_preview(&video.path) {
			preview_cache.lock().unwrap().insert(index, preview.clone());
			return Some(create_response("image/jpeg", Cow::Owned(preview)));
		}
		None
	}

	pub fn create_wallpaper_player(window: &Window) -> Result<WallpaperPlayer, Box<dyn Error>> {
		unsafe {
			let ns_view = window.ns_view() as *mut AnyObject;
			let bounds: NSRect = msg_send![ns_view, bounds];
			let layer: Retained<AnyObject> = msg_send![
				class!(AVPlayerLayer),
				playerLayerWithPlayer: None::<&AnyObject>
			];
			let black_color: *mut AnyObject = msg_send![class!(NSColor), blackColor];
			let black_cg_color: *mut AnyObject = msg_send![black_color, CGColor];
			let _: () = msg_send![ns_view, setWantsLayer: Bool::YES];
			let _: () = msg_send![&*layer, setBackgroundColor: black_cg_color];
			let _: () = msg_send![&*layer, setFrame: bounds];
			let _: () = msg_send![ns_view, setLayer: &*layer];
			Ok(WallpaperPlayer { layer, looper: None, player: None, view: ns_view })
		}
	}

	pub fn create_wallpaper_window(
		event_loop: &EventLoopWindowTarget<UserEvent>
	) -> Result<Window, Box<dyn Error>> {
		let monitor = event_loop.primary_monitor().unwrap();
		let monitor_size = PhysicalSize::new(monitor.size().width, monitor.size().height);
		Ok(WindowBuilder::new()
			.with_decorations(false)
			.with_inner_size(monitor_size)
			.with_position(monitor.position())
			.with_resizable(false)
			.with_title("PaperWall Wallpaper")
			.with_visible(false)
			.with_visible_on_all_workspaces(true)
			.build(event_loop)?)
	}

	pub fn prepare_preview_cache(_videos: &[Video]) -> Result<(), Box<dyn Error>> {
		Ok(())
	}

	pub fn read_default_video_folder() -> Result<PathBuf, Box<dyn Error>> {
		let directories = NSSearchPathForDirectoriesInDomains(
			NSSearchPathDirectory::MoviesDirectory,
			NSSearchPathDomainMask::UserDomainMask,
			true
		);
		if let Some(video_folder) = directories.firstObject() {
			return Ok(PathBuf::from(video_folder.to_string()));
		}
		Ok(read_home_dir()?.join("Movies"))
	}

	pub fn read_home_dir() -> Result<PathBuf, Box<dyn Error>> {
		let home_dir = env::var_os("HOME").ok_or("HOME is unavailable")?;
		Ok(PathBuf::from(home_dir))
	}

	pub fn read_settings_path() -> Result<PathBuf, Box<dyn Error>> {
		Ok(read_home_dir()?
			.join("Library")
			.join("Application Support")
			.join("paperwall")
			.join("settings.json"))
	}

	pub fn show_wallpaper_window(window: &Window) {
		unsafe {
			let ns_window = window.ns_window() as *mut AnyObject;
			let _: () = msg_send![ns_window, orderFront: None::<&AnyObject>];
		}
	}

	fn create_preview(path: &Path) -> Result<Vec<u8>, Box<dyn Error>> {
		let path = path.to_str().ok_or("Video path is not UTF-8")?;
		let path = NSString::from_str(path);
		let url = NSURL::fileURLWithPath(&path);
		unsafe {
			let asset: Retained<AnyObject> = msg_send![class!(AVURLAsset), assetWithURL: &*url];
			let generator: Retained<AnyObject> = msg_send![
				class!(AVAssetImageGenerator),
				assetImageGeneratorWithAsset: &*asset
			];
			let time = create_preview_time(&asset);
			let _: () = msg_send![&*generator, setAppliesPreferredTrackTransform: Bool::YES];
			let image: *mut CGImage = msg_send![
				&*generator,
				copyCGImageAtTime: time,
				actualTime: ptr::null_mut::<CMTime>(),
				error: ptr::null_mut::<*mut AnyObject>()
			];
			if image.is_null() {
				return Err("Could not create video preview".into());
			}
			let bitmap = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), &*image);
			let properties = NSDictionary::<NSBitmapImageRepPropertyKey, AnyObject>::new();
			let data = bitmap
				.representationUsingType_properties(NSBitmapImageFileType::JPEG, &properties);
			CGImageRelease(image);
			Ok(data.ok_or("Could not encode video preview")?.to_vec())
		}
	}

	fn create_preview_time(asset: &AnyObject) -> CMTime {
		unsafe {
			let duration: CMTime = msg_send![asset, duration];
			if duration.value <= 0 || duration.timescale <= 0 {
				return CMTimeMake(0, 1);
			}
			let duration_seconds = CMTimeGetSeconds(duration);
			let tracks: *mut AnyObject = msg_send![asset, tracksWithMediaType: &*AV_MEDIA_TYPE_VIDEO];
			let track_count: usize = msg_send![tracks, count];
			if track_count > 0 && duration_seconds.is_finite() {
				let track: *mut AnyObject = msg_send![tracks, objectAtIndex: 0usize];
				let frame_rate: f32 = msg_send![track, nominalFrameRate];
				if frame_rate > 0.0 {
					let frame_count = (duration_seconds * f64::from(frame_rate)).floor();
					let middle_frame = (frame_count / 2.0).floor();
					let seconds = middle_frame / f64::from(frame_rate);
					let value = (seconds * f64::from(duration.timescale)).floor() as i64;
					return CMTimeMake(value, duration.timescale);
				}
			}
			CMTimeMake(duration.value / 2, duration.timescale)
		}
	}

	impl ScalingMode {
		fn video_gravity(self) -> &'static AnyObject {
			unsafe {
				match self {
					Self::FillScreen => &*AV_LAYER_VIDEO_GRAVITY_RESIZE_ASPECT_FILL,
					Self::FitToScreen => &*AV_LAYER_VIDEO_GRAVITY_RESIZE_ASPECT
				}
			}
		}
	}

	impl WallpaperPlayer {
		pub fn needs_poll(&self) -> bool {
			false
		}

		pub fn poll(&mut self) -> Result<(), Box<dyn Error>> {
			Ok(())
		}

		pub fn play(
			&mut self,
			path: &Path,
			scaling_mode: ScalingMode
		) -> Result<(), Box<dyn Error>> {
			let path = path.to_str().ok_or("Video path is not UTF-8")?;
			let path = NSString::from_str(path);
			let url = NSURL::fileURLWithPath(&path);
			let item: Retained<AnyObject> = unsafe {
				msg_send![class!(AVPlayerItem), playerItemWithURL: &*url]
			};
			let player: Retained<AnyObject> = unsafe { msg_send![class!(AVQueuePlayer), new] };
			let looper: Retained<AnyObject> = unsafe {
				msg_send![
					class!(AVPlayerLooper),
					playerLooperWithPlayer: &*player,
					templateItem: &*item
				]
			};
			unsafe {
				let bounds: NSRect = msg_send![self.view, bounds];
				let _: () = msg_send![&*self.layer, setFrame: bounds];
				let _: () = msg_send![&*player, setMuted: Bool::YES];
				let _: () = msg_send![&*self.layer, setPlayer: &*player];
				let _: () = msg_send![&*player, play];
			}
			self.looper = Some(looper);
			self.player = Some(player);
			self.set_scaling_mode(scaling_mode);
			Ok(())
		}

		pub fn set_scaling_mode(&mut self, scaling_mode: ScalingMode) {
			let gravity = scaling_mode.video_gravity();
			unsafe {
				let _: () = msg_send![&*self.layer, setVideoGravity: gravity];
			}
		}
	}
}

#[cfg(target_os = "windows")]
mod platform {
	use super::*;
	use std::collections::HashSet;
	use std::env;
	use std::mem::ManuallyDrop;
	use std::os::windows::ffi::OsStrExt;
	use std::time::UNIX_EPOCH;
	use tao::dpi::PhysicalSize;
	use tao::platform::windows::{WindowBuilderExtWindows, WindowExtWindows};
	use windows::core::{BOOL, GUID, IUnknown, Interface, PCWSTR};
	use windows::Win32::Foundation::{
		COLORREF, HWND, LPARAM, RECT, RPC_E_CHANGED_MODE, S_FALSE, S_OK, SIZE, WPARAM
	};
	use windows::Win32::Graphics::Imaging::{
		CLSID_WICImagingFactory, GUID_ContainerFormatJpeg, GUID_WICPixelFormat24bppBGR,
		GUID_WICPixelFormat32bppBGRA, IWICImagingFactory, IWICPalette, WICBitmapDitherTypeNone,
		WICBitmapEncoderNoCache, WICBitmapPaletteTypeCustom
	};
	use windows::Win32::Media::MediaFoundation::{
		IMFAttributes, IMFMediaEvent, IMFMediaSession, IMFMediaSource, IMFSample,
		IMFStreamDescriptor, IMFTopology, IMFTopologyNode, IMFVideoDisplayControl, MEError,
		MEEndOfPresentation, MESessionEnded, MESessionTopologyStatus, MFCreateAttributes,
		MFCreateMediaSession, MFCreateMediaType, MFCreateSourceReaderFromURL,
		MFCreateSourceResolver, MFCreateTopology, MFCreateTopologyNode,
		MFCreateVideoRendererActivate, MFMediaType_Video, MFShutdown, MFStartup,
		MFSTARTUP_LITE, MFVideoARMode_None, MFVideoFormat_RGB32, MFVideoNormalizedRect,
		MF_E_NO_EVENTS_AVAILABLE, MF_EVENT_FLAG_NO_WAIT, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE,
		MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_OBJECT_MEDIASOURCE, MF_OBJECT_TYPE,
		MF_PD_DURATION, MF_RESOLUTION_MEDIASOURCE,
		MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED, MF_SOURCE_READERF_ENDOFSTREAM,
		MF_SOURCE_READERF_NATIVEMEDIATYPECHANGED, MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING,
		MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_SOURCE_READER_MEDIASOURCE,
		MF_TOPOLOGY_OUTPUT_NODE, MF_TOPOLOGY_SOURCESTREAM_NODE,
		MF_TOPONODE_PRESENTATION_DESCRIPTOR, MF_TOPONODE_SOURCE,
		MF_TOPONODE_STREAM_DESCRIPTOR, MF_TOPOSTATUS_READY, MF_VERSION, MFGetService,
		MR_VIDEO_RENDER_SERVICE
	};
	use windows::Win32::System::Com::StructuredStorage::{
		PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0, PropVariantClear,
		PropVariantToInt32, PropVariantToInt64
	};
	use windows::Win32::System::Com::{
		CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
		CoTaskMemFree, CoUninitialize, STGM_CREATE, STGM_WRITE
	};
	use windows::Win32::System::Variant::VT_I8;
	use windows::Win32::UI::Shell::{FOLDERID_Videos, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
	use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
	use windows::Win32::UI::WindowsAndMessaging::{
		EnumWindows, FindWindowExW, FindWindowW, GetClientRect, GetWindowLongPtrW, SendMessageTimeoutW,
		SetParent, SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, HWND_BOTTOM, SMTO_NORMAL,
		SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, WS_EX_NOACTIVATE,
		WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT
	};

	const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
	const FNV_PRIME: u64 = 0x100000001b3;
	const MEDIA_TIME_UNITS_PER_SECOND: f64 = 10_000_000.0;
	const WORKERW_MESSAGE: u32 = 0x052C;
	const WORKERW_TIMEOUT_MS: u32 = 1000;

	struct ComApartment {
		should_uninitialize: bool
	}

	struct MediaFoundation;

	pub struct WallpaperPlayer {
		_com: ComApartment,
		_mf: MediaFoundation,
		hwnd: HWND,
		scaling_mode: ScalingMode,
		session: Option<IMFMediaSession>,
		source: Option<IMFMediaSource>
	}

	pub fn configure_wallpaper_window(window: &Window) -> Result<(), Box<dyn Error>> {
		let _ = window.set_ignore_cursor_events(true);
		let hwnd = HWND(window.hwnd() as *mut _);
		unsafe {
			let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
			let style = style
				| WS_EX_NOACTIVATE.0 as isize
				| WS_EX_TOOLWINDOW.0 as isize
				| WS_EX_TRANSPARENT.0 as isize;
			let _ = SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style);
			if let Some(desktop_window) = find_desktop_window() {
				let _ = SetParent(hwnd, Some(desktop_window));
			}
		}
		Ok(())
	}

	pub fn create_preview_response(
		request: &Request<Vec<u8>>,
		videos: &[Video],
		preview_cache: &Arc<Mutex<HashMap<usize, Vec<u8>>>>
	) -> Option<Response<Cow<'static, [u8]>>> {
		let index = request
			.uri()
			.path()
			.strip_prefix("/preview/")
			.and_then(|name| name.strip_suffix(".jpg"))
			.and_then(|name| name.parse::<usize>().ok())?;
		if let Some(preview) = preview_cache.lock().unwrap().get(&index).cloned() {
			return Some(create_response("image/jpeg", Cow::Owned(preview)));
		}
		let video = videos.get(index)?;
		Some(match read_preview(video) {
			Ok(preview) => {
				preview_cache.lock().unwrap().insert(index, preview.clone());
				create_response("image/jpeg", Cow::Owned(preview))
			}
			Err(error) => create_error_response(error)
		})
	}

	pub fn create_wallpaper_player(window: &Window) -> Result<WallpaperPlayer, Box<dyn Error>> {
		Ok(WallpaperPlayer {
			_com: ComApartment::new()?,
			_mf: MediaFoundation::new()?,
			hwnd: HWND(window.hwnd() as *mut _),
			scaling_mode: ScalingMode::FillScreen,
			session: None,
			source: None
		})
	}

	pub fn create_wallpaper_window(
		event_loop: &EventLoopWindowTarget<UserEvent>
	) -> Result<Window, Box<dyn Error>> {
		let monitor = event_loop.primary_monitor().unwrap();
		let monitor_size = PhysicalSize::new(monitor.size().width, monitor.size().height);
		Ok(WindowBuilder::new()
			.with_decorations(false)
			.with_inner_size(monitor_size)
			.with_position(monitor.position())
			.with_resizable(false)
			.with_skip_taskbar(true)
			.with_title("PaperWall Wallpaper")
			.with_undecorated_shadow(false)
			.with_visible(false)
			.build(event_loop)?)
	}

	pub fn prepare_preview_cache(videos: &[Video]) -> Result<(), Box<dyn Error>> {
		let cache_dir = read_cache_dir()?;
		let mut desired_file_names = HashSet::new();
		for video in videos {
			desired_file_names.insert(create_cache_file_name(video)?);
		}
		let entries = match fs::read_dir(&cache_dir) {
			Ok(entries) => entries,
			Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
			Err(error) => return Err(error.into())
		};
		for entry in entries {
			let path = entry?.path();
			let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
				continue;
			};
			let is_stale_preview = name.ends_with(".jpg") && !desired_file_names.contains(name);
			let is_temp_preview = name.ends_with(".tmp");
			if name.starts_with("preview-") && (is_stale_preview || is_temp_preview) {
				let _ = fs::remove_file(path);
			}
		}
		Ok(())
	}

	pub fn read_default_video_folder() -> Result<PathBuf, Box<dyn Error>> {
		let path = unsafe { SHGetKnownFolderPath(&FOLDERID_Videos, KF_FLAG_DEFAULT, None) }?;
		let video_folder = unsafe { path.to_string() };
		unsafe {
			CoTaskMemFree(Some(path.as_ptr().cast()));
		}
		Ok(PathBuf::from(video_folder?))
	}

	pub fn read_settings_path() -> Result<PathBuf, Box<dyn Error>> {
		let local_app_data = env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is unavailable")?;
		Ok(PathBuf::from(local_app_data)
			.join("paperwall")
			.join("settings.json"))
	}

	pub fn show_wallpaper_window(window: &Window) {
		let hwnd = HWND(window.hwnd() as *mut _);
		unsafe {
			let _ = SetWindowPos(
				hwnd,
				Some(HWND_BOTTOM),
				0,
				0,
				0,
				0,
				SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW
			);
		}
	}

	fn create_error_response(error: Box<dyn Error>) -> Response<Cow<'static, [u8]>> {
		Response::builder()
			.header(CONTENT_TYPE, "text/plain")
			.status(500)
			.body(Cow::Owned(error.to_string().into_bytes()))
			.unwrap()
	}

	fn create_cache_file_name(video: &Video) -> Result<String, Box<dyn Error>> {
		let metadata = fs::metadata(&video.path)?;
		let modified = metadata.modified()?.duration_since(UNIX_EPOCH)?;
		let mut hash = FNV_OFFSET_BASIS;
		for unit in video.path.as_os_str().encode_wide() {
			hash_bytes(&mut hash, &unit.to_le_bytes());
		}
		hash_bytes(&mut hash, &metadata.len().to_le_bytes());
		hash_bytes(&mut hash, &modified.as_secs().to_le_bytes());
		hash_bytes(&mut hash, &modified.subsec_nanos().to_le_bytes());
		Ok(format!("preview-{hash:016x}.jpg"))
	}

	fn create_position(timestamp: i64) -> PROPVARIANT {
		PROPVARIANT {
			Anonymous: PROPVARIANT_0 {
				Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
					vt: VT_I8,
					wReserved1: 0,
					wReserved2: 0,
					wReserved3: 0,
					Anonymous: PROPVARIANT_0_0_0 { hVal: timestamp }
				})
			}
		}
	}

	fn create_preview(video_path: &Path, cache_path: &Path) -> Result<(), Box<dyn Error>> {
		let _com = ComApartment::new()?;
		let _mf = MediaFoundation::new()?;
		let (width, height, pixels) = decode_preview(video_path)?;
		write_jpeg(cache_path, width, height, &pixels)?;
		Ok(())
	}

	fn create_source_reader_attributes() -> Result<IMFAttributes, Box<dyn Error>> {
		let mut attributes = None;
		unsafe {
			MFCreateAttributes(&mut attributes, 1)?;
		}
		let attributes = attributes.ok_or("Could not create media attributes")?;
		unsafe {
			attributes.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?;
		}
		Ok(attributes)
	}

	fn create_media_source(path: &Path) -> Result<IMFMediaSource, Box<dyn Error>> {
		let resolver = unsafe { MFCreateSourceResolver()? };
		let path = wide_path(path);
		let mut object = None;
		let mut object_type = MF_OBJECT_TYPE::default();
		unsafe {
			resolver.CreateObjectFromURL(
				PCWSTR::from_raw(path.as_ptr()),
				MF_RESOLUTION_MEDIASOURCE.0 as u32,
				None::<&IPropertyStore>,
				&mut object_type,
				&mut object
			)?;
		}
		if object_type != MF_OBJECT_MEDIASOURCE {
			return Err("Media Foundation did not create a media source".into());
		}
		Ok(object.ok_or("Could not create media source")?.cast()?)
	}

	fn create_output_node(hwnd: HWND) -> Result<IMFTopologyNode, Box<dyn Error>> {
		let activate = unsafe { MFCreateVideoRendererActivate(hwnd)? };
		let output_node = unsafe { MFCreateTopologyNode(MF_TOPOLOGY_OUTPUT_NODE)? };
		unsafe {
			output_node.SetObject(&activate)?;
		}
		Ok(output_node)
	}

	fn create_source_node(
		source: &IMFMediaSource,
		presentation: &windows::Win32::Media::MediaFoundation::IMFPresentationDescriptor,
		stream: &IMFStreamDescriptor
	) -> Result<IMFTopologyNode, Box<dyn Error>> {
		let source_node = unsafe { MFCreateTopologyNode(MF_TOPOLOGY_SOURCESTREAM_NODE)? };
		unsafe {
			source_node.SetUnknown(&MF_TOPONODE_SOURCE, source)?;
			source_node.SetUnknown(&MF_TOPONODE_PRESENTATION_DESCRIPTOR, presentation)?;
			source_node.SetUnknown(&MF_TOPONODE_STREAM_DESCRIPTOR, stream)?;
		}
		Ok(source_node)
	}

	fn create_topology(source: &IMFMediaSource, hwnd: HWND) -> Result<IMFTopology, Box<dyn Error>> {
		let presentation = unsafe { source.CreatePresentationDescriptor()? };
		let stream = read_video_stream(&presentation)?;
		let topology = unsafe { MFCreateTopology()? };
		let source_node = create_source_node(source, &presentation, &stream)?;
		let output_node = create_output_node(hwnd)?;
		unsafe {
			topology.AddNode(&source_node)?;
			topology.AddNode(&output_node)?;
			source_node.ConnectOutput(0, &output_node, 0)?;
		}
		Ok(topology)
	}

	fn decode_preview(path: &Path) -> Result<(u32, u32, Vec<u8>), Box<dyn Error>> {
		let source_path = wide_path(path);
		let attributes = create_source_reader_attributes()?;
		let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
		let reader = unsafe {
			MFCreateSourceReaderFromURL(
				PCWSTR::from_raw(source_path.as_ptr()),
				&attributes
			)?
		};
		let media_type = unsafe { MFCreateMediaType()? };
		unsafe {
			media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
			media_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
			reader.SetStreamSelection(stream, true)?;
			reader.SetCurrentMediaType(stream, None, &media_type)?;
		}
		let current_media_type = unsafe { reader.GetCurrentMediaType(stream)? };
		let (width, height) = read_frame_size(&current_media_type)?;
		let position = create_position(read_preview_timestamp(&reader, &current_media_type));
		unsafe {
			reader.SetCurrentPosition(&GUID::zeroed(), &position)?;
		}
		read_next_frame(&reader, stream, width, height)
	}

	fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
		for byte in bytes {
			*hash ^= u64::from(*byte);
			*hash = hash.wrapping_mul(FNV_PRIME);
		}
	}

	fn read_cache_dir() -> Result<PathBuf, Box<dyn Error>> {
		let local_app_data = env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is unavailable")?;
		Ok(PathBuf::from(local_app_data).join("paperwall").join("cache"))
	}

	fn read_duration(reader: &windows::Win32::Media::MediaFoundation::IMFSourceReader) -> Option<i64> {
		let mut propvariant = unsafe {
			reader
				.GetPresentationAttribute(
					MF_SOURCE_READER_MEDIASOURCE.0 as u32,
					&MF_PD_DURATION
				)
				.ok()?
		};
		let duration = unsafe { PropVariantToInt64(&propvariant).ok()? };
		unsafe {
			PropVariantClear(&mut propvariant).ok()?;
		}
		Some(duration)
	}

	fn read_frame_rate(
		media_type: &windows::Win32::Media::MediaFoundation::IMFMediaType
	) -> Option<f64> {
		let frame_rate = unsafe { media_type.GetUINT64(&MF_MT_FRAME_RATE).ok()? };
		let numerator = frame_rate >> 32;
		let denominator = frame_rate & u64::from(u32::MAX);
		if numerator == 0 || denominator == 0 {
			return None;
		}
		Some(numerator as f64 / denominator as f64)
	}

	fn read_frame_size(
		media_type: &windows::Win32::Media::MediaFoundation::IMFMediaType
	) -> Result<(u32, u32), Box<dyn Error>> {
		let frame_size = unsafe { media_type.GetUINT64(&MF_MT_FRAME_SIZE)? };
		Ok(((frame_size >> 32) as u32, frame_size as u32))
	}

	fn read_display_control(
		session: &IMFMediaSession
	) -> Result<IMFVideoDisplayControl, Box<dyn Error>> {
		let mut pointer = std::ptr::null_mut();
		unsafe {
			MFGetService(
				session,
				&MR_VIDEO_RENDER_SERVICE,
				&IMFVideoDisplayControl::IID,
				&mut pointer
			)?;
			Ok(IMFVideoDisplayControl::from_raw(pointer))
		}
	}

	fn read_event_value(event: &IMFMediaEvent) -> Option<i32> {
		let mut value = unsafe { event.GetValue().ok()? };
		let result = unsafe { PropVariantToInt32(&value).ok() };
		unsafe {
			PropVariantClear(&mut value).ok()?;
		}
		result
	}

	fn read_next_frame(
		reader: &windows::Win32::Media::MediaFoundation::IMFSourceReader,
		stream: u32,
		width: u32,
		height: u32
	) -> Result<(u32, u32, Vec<u8>), Box<dyn Error>> {
		let end_of_stream = MF_SOURCE_READERF_ENDOFSTREAM.0 as u32;
		let type_changed = MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 as u32
			| MF_SOURCE_READERF_NATIVEMEDIATYPECHANGED.0 as u32;
		loop {
			let mut flags = 0;
			let mut sample = None;
			unsafe {
				reader.ReadSample(
					stream,
					0,
					None,
					Some(&mut flags),
					None,
					Some(&mut sample)
				)?;
			}
			if flags & end_of_stream != 0 {
				return Err("Could not read video preview".into());
			}
			if flags & type_changed != 0 {
				continue;
			}
			if let Some(sample) = sample {
				return Ok((width, height, read_sample_pixels(&sample, width, height)?));
			}
		}
	}

	fn read_video_stream(
		presentation: &windows::Win32::Media::MediaFoundation::IMFPresentationDescriptor
	) -> Result<IMFStreamDescriptor, Box<dyn Error>> {
		let stream_count = unsafe { presentation.GetStreamDescriptorCount()? };
		for index in 0..stream_count {
			let mut selected = BOOL(0);
			let mut stream = None;
			unsafe {
				presentation.GetStreamDescriptorByIndex(index, &mut selected, &mut stream)?;
			}
			let stream = stream.ok_or("Could not read stream descriptor")?;
			let media_type_handler = unsafe { stream.GetMediaTypeHandler()? };
			if unsafe { media_type_handler.GetMajorType()? } == MFMediaType_Video {
				unsafe {
					presentation.SelectStream(index)?;
				}
				return Ok(stream);
			}
			unsafe {
				presentation.DeselectStream(index)?;
			}
		}
		Err("Could not find a video stream".into())
	}

	fn read_preview(video: &Video) -> Result<Vec<u8>, Box<dyn Error>> {
		let cache_dir = read_cache_dir()?;
		let cache_path = cache_dir.join(create_cache_file_name(video)?);
		match fs::read(&cache_path) {
			Ok(preview) => return Ok(preview),
			Err(error) if error.kind() == ErrorKind::NotFound => {}
			Err(error) => return Err(error.into())
		}
		fs::create_dir_all(&cache_dir)?;
		let temp_path = cache_path.with_extension("jpg.tmp");
		let _ = fs::remove_file(&temp_path);
		if let Err(error) = create_preview(&video.path, &temp_path) {
			let _ = fs::remove_file(&temp_path);
			return Err(error);
		}
		fs::rename(&temp_path, &cache_path)?;
		Ok(fs::read(cache_path)?)
	}

	fn read_preview_timestamp(
		reader: &windows::Win32::Media::MediaFoundation::IMFSourceReader,
		media_type: &windows::Win32::Media::MediaFoundation::IMFMediaType
	) -> i64 {
		let Some(duration) = read_duration(reader) else {
			return 0;
		};
		if duration <= 0 {
			return 0;
		}
		if let Some(frame_rate) = read_frame_rate(media_type) {
			let duration_seconds = duration as f64 / MEDIA_TIME_UNITS_PER_SECOND;
			let frame_count = (duration_seconds * frame_rate).floor();
			if frame_count > 0.0 {
				let middle_frame = (frame_count / 2.0).floor();
				return (middle_frame / frame_rate * MEDIA_TIME_UNITS_PER_SECOND).floor() as i64;
			}
		}
		duration / 2
	}

	fn read_sample_pixels(
		sample: &IMFSample,
		width: u32,
		height: u32
	) -> Result<Vec<u8>, Box<dyn Error>> {
		let buffer = unsafe { sample.ConvertToContiguousBuffer()? };
		let frame_len = (width as usize)
			.checked_mul(height as usize)
			.and_then(|pixels| pixels.checked_mul(4))
			.ok_or("Video preview is too large")?;
		let mut bytes: *mut u8 = std::ptr::null_mut();
		let mut current_len = 0;
		unsafe {
			buffer.Lock(&mut bytes, None, Some(&mut current_len))?;
		}
		let pixels = if bytes.is_null() || (current_len as usize) < frame_len {
			Err("Video preview has too few bytes".into())
		} else {
			Ok(unsafe { std::slice::from_raw_parts(bytes, frame_len).to_vec() })
		};
		unsafe {
			buffer.Unlock()?;
		}
		pixels
	}

	fn update_video_position(
		display: &IMFVideoDisplayControl,
		hwnd: HWND,
		scaling_mode: ScalingMode
	) -> Result<(), Box<dyn Error>> {
		let mut native_size = SIZE::default();
		let mut aspect_size = SIZE::default();
		let mut client = RECT::default();
		unsafe {
			display.GetNativeVideoSize(&mut native_size, &mut aspect_size)?;
			GetClientRect(hwnd, &mut client)?;
		}
		let video_width = if aspect_size.cx > 0 { aspect_size.cx } else { native_size.cx };
		let video_height = if aspect_size.cy > 0 { aspect_size.cy } else { native_size.cy };
		let client_width = client.right - client.left;
		let client_height = client.bottom - client.top;
		if video_width <= 0 || video_height <= 0 || client_width <= 0 || client_height <= 0 {
			return Ok(());
		}
		let video_width = f64::from(video_width);
		let video_height = f64::from(video_height);
		let client_width = f64::from(client_width);
		let client_height = f64::from(client_height);
		let full_source = MFVideoNormalizedRect {
			left: 0.0,
			top: 0.0,
			right: 1.0,
			bottom: 1.0
		};
		let (source, destination) = match scaling_mode {
			ScalingMode::FillScreen => {
				let scale = (client_width / video_width).max(client_height / video_height);
				let visible_width = (client_width / (video_width * scale)).min(1.0);
				let visible_height = (client_height / (video_height * scale)).min(1.0);
				let left = ((1.0 - visible_width) / 2.0) as f32;
				let top = ((1.0 - visible_height) / 2.0) as f32;
				(
					MFVideoNormalizedRect {
						left,
						top,
						right: 1.0 - left,
						bottom: 1.0 - top
					},
					client
				)
			}
			ScalingMode::FitToScreen => {
				let scale = (client_width / video_width).min(client_height / video_height);
				let width = (video_width * scale).round() as i32;
				let height = (video_height * scale).round() as i32;
				let left = ((client_width as i32) - width) / 2;
				let top = ((client_height as i32) - height) / 2;
				(
					full_source,
					RECT {
						left,
						top,
						right: left + width,
						bottom: top + height
					}
				)
			}
		};
		unsafe {
			display.SetAspectRatioMode(MFVideoARMode_None.0 as u32)?;
			display.SetBorderColor(COLORREF(0))?;
			display.SetVideoPosition(&source, &destination)?;
			display.RepaintVideo()?;
		}
		Ok(())
	}

	fn find_desktop_window() -> Option<HWND> {
		unsafe {
			let progman_class = wide("Progman");
			let progman = FindWindowW(
				PCWSTR::from_raw(progman_class.as_ptr()),
				PCWSTR::null()
			)
			.ok()?;
			let mut result = 0;
			let _ = SendMessageTimeoutW(
				progman,
				WORKERW_MESSAGE,
				WPARAM(0),
				LPARAM(0),
				SMTO_NORMAL,
				WORKERW_TIMEOUT_MS,
				Some(&mut result)
			);
			let mut desktop_window = HWND::default();
			let _ = EnumWindows(
				Some(read_worker_window),
				LPARAM(&mut desktop_window as *mut HWND as isize)
			);
			if desktop_window.is_invalid() {
				return Some(progman);
			}
			Some(desktop_window)
		}
	}

	unsafe extern "system" fn read_worker_window(window: HWND, lparam: LPARAM) -> BOOL {
		let shell_class = wide("SHELLDLL_DefView");
		let shell_window = unsafe {
			FindWindowExW(
				Some(window),
				None,
				PCWSTR::from_raw(shell_class.as_ptr()),
				PCWSTR::null()
			)
		};
		if shell_window.is_ok() {
			let worker_class = wide("WorkerW");
			if let Ok(worker_window) = unsafe {
				FindWindowExW(
					None,
					Some(window),
					PCWSTR::from_raw(worker_class.as_ptr()),
					PCWSTR::null()
				)
			} {
				if !worker_window.is_invalid() {
					unsafe {
						*(lparam.0 as *mut HWND) = worker_window;
					}
					return BOOL(0);
				}
			}
		}
		BOOL(1)
	}

	fn wide_path(path: &Path) -> Vec<u16> {
		path.as_os_str().encode_wide().chain([0]).collect()
	}

	fn write_jpeg(
		path: &Path,
		width: u32,
		height: u32,
		pixels: &[u8]
	) -> Result<(), Box<dyn Error>> {
		let stride = width.checked_mul(4).ok_or("Video preview is too wide")?;
		let factory: IWICImagingFactory = unsafe {
			CoCreateInstance(&CLSID_WICImagingFactory, None::<&IUnknown>, CLSCTX_INPROC_SERVER)?
		};
		let path = wide_path(path);
		unsafe {
			let stream = factory.CreateStream()?;
			stream.InitializeFromFilename(
				PCWSTR::from_raw(path.as_ptr()),
				(STGM_CREATE | STGM_WRITE).0
			)?;
			let encoder = factory.CreateEncoder(&GUID_ContainerFormatJpeg, std::ptr::null())?;
			encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
			let mut frame = None;
			let mut options = None;
			encoder.CreateNewFrame(&mut frame, &mut options)?;
			let frame = frame.ok_or("Could not create JPEG frame")?;
			frame.Initialize(options.as_ref())?;
			frame.SetSize(width, height)?;
			let bitmap = factory.CreateBitmapFromMemory(
				width,
				height,
				&GUID_WICPixelFormat32bppBGRA,
				stride,
				pixels
			)?;
			let converter = factory.CreateFormatConverter()?;
			converter.Initialize(
				&bitmap,
				&GUID_WICPixelFormat24bppBGR,
				WICBitmapDitherTypeNone,
				None::<&IWICPalette>,
				0.0,
				WICBitmapPaletteTypeCustom
			)?;
			let mut pixel_format = GUID_WICPixelFormat24bppBGR;
			frame.SetPixelFormat(&mut pixel_format)?;
			frame.WriteSource(&converter, std::ptr::null())?;
			frame.Commit()?;
			encoder.Commit()?;
		}
		Ok(())
	}

	fn wide(text: &str) -> Vec<u16> {
		text.encode_utf16().chain([0]).collect()
	}

	impl ComApartment {
		fn new() -> Result<Self, Box<dyn Error>> {
			let result = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
			if result == RPC_E_CHANGED_MODE {
				return Ok(Self { should_uninitialize: false });
			}
			result.ok()?;
			Ok(Self { should_uninitialize: result == S_OK || result == S_FALSE })
		}
	}

	impl Drop for ComApartment {
		fn drop(&mut self) {
			if self.should_uninitialize {
				unsafe {
					CoUninitialize();
				}
			}
		}
	}

	impl Drop for MediaFoundation {
		fn drop(&mut self) {
			let _ = unsafe { MFShutdown() };
		}
	}

	impl MediaFoundation {
		fn new() -> Result<Self, Box<dyn Error>> {
			unsafe {
				MFStartup(MF_VERSION, MFSTARTUP_LITE)?;
			}
			Ok(Self)
		}
	}

	impl WallpaperPlayer {
		fn apply_scaling_mode(&self) -> Result<(), Box<dyn Error>> {
			let Some(session) = &self.session else {
				return Ok(());
			};
			let display = read_display_control(session)?;
			update_video_position(&display, self.hwnd, self.scaling_mode)
		}

		fn close_session(&mut self) {
			if let Some(session) = self.session.take() {
				unsafe {
					let _ = session.Stop();
					let _ = session.Close();
					let _ = session.Shutdown();
				}
			}
			if let Some(source) = self.source.take() {
				unsafe {
					let _ = source.Shutdown();
				}
			}
		}

		pub fn needs_poll(&self) -> bool {
			self.session.is_some()
		}

		pub fn play(
			&mut self,
			path: &Path,
			scaling_mode: ScalingMode
		) -> Result<(), Box<dyn Error>> {
			self.close_session();
			self.scaling_mode = scaling_mode;
			let source = create_media_source(path)?;
			let topology = create_topology(&source, self.hwnd)?;
			let session = unsafe { MFCreateMediaSession(None::<&IMFAttributes>)? };
			let position = create_position(0);
			unsafe {
				session.SetTopology(0, &topology)?;
				session.Start(&GUID::zeroed(), &position)?;
			}
			self.source = Some(source);
			self.session = Some(session);
			let _ = self.apply_scaling_mode();
			Ok(())
		}

		pub fn poll(&mut self) -> Result<(), Box<dyn Error>> {
			loop {
				let result = {
					let Some(session) = &self.session else {
						return Ok(());
					};
					unsafe { session.GetEvent(MF_EVENT_FLAG_NO_WAIT) }
				};
				let event = match result {
					Ok(event) => event,
					Err(error) if error.code() == MF_E_NO_EVENTS_AVAILABLE => return Ok(()),
					Err(error) => return Err(error.into())
				};
				unsafe {
					event.GetStatus()?.ok()?;
				}
				let event_type = unsafe { event.GetType()? };
				if event_type == MEError.0 as u32 {
					return Err("Media Foundation playback error".into());
				}
				if event_type == MEEndOfPresentation.0 as u32
					|| event_type == MESessionEnded.0 as u32
				{
					self.restart()?;
				}
				if event_type == MESessionTopologyStatus.0 as u32
					&& read_event_value(&event) == Some(MF_TOPOSTATUS_READY.0)
				{
					self.apply_scaling_mode()?;
				}
			}
		}

		fn restart(&self) -> Result<(), Box<dyn Error>> {
			if let Some(session) = &self.session {
				let position = create_position(0);
				unsafe {
					session.Start(&GUID::zeroed(), &position)?;
				}
			}
			Ok(())
		}

		pub fn set_scaling_mode(&mut self, scaling_mode: ScalingMode) {
			self.scaling_mode = scaling_mode;
			let _ = self.apply_scaling_mode();
		}
	}

	impl Drop for WallpaperPlayer {
		fn drop(&mut self) {
			self.close_session();
		}
	}
}

fn main() -> Result<(), Box<dyn Error>> {
	run()
}
