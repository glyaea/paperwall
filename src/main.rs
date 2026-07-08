#[cfg(not(target_os = "macos"))]
compile_error!("PaperWall currently supports macOS.");

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
	platform::configure_wallpaper_window(&wallpaper_window)?;
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

fn main() -> Result<(), Box<dyn Error>> {
	run()
}
