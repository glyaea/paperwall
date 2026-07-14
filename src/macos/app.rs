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
use std::borrow::Cow;
use std::collections::HashMap;
use std::env;
use std::ptr;
use tao::dpi::PhysicalSize;
use tao::event_loop::{EventLoopProxy, EventLoopWindowTarget};
use tao::platform::macos::WindowExtMacOS;
use wry::http::{header::CONTENT_TYPE, Request, Response};
use wry::{WebView, WebViewBuilder};

const APP_JS: &str = include_str!("app.js");
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

pub struct Picker {
	html: Arc<Mutex<String>>,
	preview_cache: Arc<Mutex<HashMap<usize, Vec<u8>>>>,
	webview: WebView,
	window: Window
}

pub struct Wallpaper {
	player: WallpaperPlayer,
	_window: Window
}

struct WallpaperPlayer {
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

fn configure_wallpaper_window(window: &Window) -> Result<(), Box<dyn Error>> {
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

fn create_asset_response(
	request: Request<Vec<u8>>,
	html: &Arc<Mutex<String>>,
	videos: &Arc<Mutex<Vec<Video>>>,
	preview_cache: &Arc<Mutex<HashMap<usize, Vec<u8>>>>
) -> Response<Cow<'static, [u8]>> {
	if request.uri().path().starts_with("/preview/") {
		if let Some(response) = create_preview_response(
			&request,
			&videos.lock().unwrap(),
			preview_cache
		) {
			return response;
		}
	}
	match request.uri().path() {
		"/" | "/main.html" => {
			let html = html.lock().unwrap();
			create_response("text/html", Cow::Owned(html.as_bytes().to_vec()))
		}
		"/app.js" => create_response("text/javascript", Cow::Borrowed(APP_JS.as_bytes())),
		"/main.css" => create_response("text/css", Cow::Borrowed(MAIN_CSS.as_bytes())),
		"/main.js" => create_response("text/javascript", Cow::Borrowed(MAIN_JS.as_bytes())),
		_ => Response::builder()
			.header(CONTENT_TYPE, "text/plain")
			.status(404)
			.body(Cow::Borrowed(&b"Not found"[..]))
			.unwrap()
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
		let data = bitmap.representationUsingType_properties(
			NSBitmapImageFileType::JPEG,
			&properties
		);
		CGImageRelease(image);
		Ok(data.ok_or("Could not encode video preview")?.to_vec())
	}
}

fn create_preview_response(
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

fn create_response(
	content_type: &str,
	body: Cow<'static, [u8]>
) -> Response<Cow<'static, [u8]>> {
	Response::builder()
		.header(CONTENT_TYPE, content_type)
		.body(body)
		.unwrap()
}

fn create_wallpaper_player(window: &Window) -> Result<WallpaperPlayer, Box<dyn Error>> {
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

fn create_wallpaper_window(
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

fn show_wallpaper_window(window: &Window) {
	unsafe {
		let ns_window = window.ns_window() as *mut AnyObject;
		let _: () = msg_send![ns_window, orderFront: None::<&AnyObject>];
	}
}

pub fn create_thumbnail(_video: &Video, index: usize) -> Result<String, Box<dyn Error>> {
	Ok(format!("<img alt=\"\" src=\"preview/{index}.jpg\">"))
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

fn read_home_dir() -> Result<PathBuf, Box<dyn Error>> {
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

impl Picker {
	pub fn create(
		window: Window,
		proxy: EventLoopProxy<UserEvent>,
		html: String,
		videos: Arc<Mutex<Vec<Video>>>
	) -> Result<Self, Box<dyn Error>> {
		let html = Arc::new(Mutex::new(html));
		let preview_cache = Arc::new(Mutex::new(HashMap::new()));
		let protocol_html = Arc::clone(&html);
		let protocol_preview_cache = Arc::clone(&preview_cache);
		let protocol_videos = Arc::clone(&videos);
		let webview = WebViewBuilder::new()
			.with_custom_protocol("paperwall".into(), move |_, request| {
				create_asset_response(
					request,
					&protocol_html,
					&protocol_videos,
					&protocol_preview_cache
				)
			})
			.with_ipc_handler(move |request: Request<String>| {
				if let Ok(event) = serde_json::from_str(request.body()) {
					let _ = proxy.send_event(event);
				}
			})
			.with_url("paperwall://localhost/main.html")
			.build(&window)?;
		Ok(Self { html, preview_cache, webview, window })
	}

	pub fn set_videos(
		&self,
		html: String,
		video_folder: &Path,
		tiles: &str
	) -> Result<(), Box<dyn Error>> {
		*self.html.lock().unwrap() = html;
		self.preview_cache.lock().unwrap().clear();
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

impl Wallpaper {
	pub fn create(
		event_loop: &EventLoopWindowTarget<UserEvent>,
		path: &Path,
		scaling_mode: ScalingMode
	) -> Result<Self, Box<dyn Error>> {
		let window = create_wallpaper_window(event_loop)?;
		configure_wallpaper_window(&window)?;
		let mut player = create_wallpaper_player(&window)?;
		player.play(path, scaling_mode)?;
		show_wallpaper_window(&window);
		Ok(Self { player, _window: window })
	}

	pub fn set_scaling_mode(
		&mut self,
		scaling_mode: ScalingMode
	) -> Result<(), Box<dyn Error>> {
		self.player.set_scaling_mode(scaling_mode);
		Ok(())
	}

	pub fn set_video(
		&mut self,
		path: &Path,
		scaling_mode: ScalingMode
	) -> Result<(), Box<dyn Error>> {
		self.player.play(path, scaling_mode)
	}
}

impl WallpaperPlayer {
	fn play(&mut self, path: &Path, scaling_mode: ScalingMode) -> Result<(), Box<dyn Error>> {
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

	fn set_scaling_mode(&mut self, scaling_mode: ScalingMode) {
		let gravity = scaling_mode.video_gravity();
		unsafe {
			let _: () = msg_send![&*self.layer, setVideoGravity: gravity];
		}
	}
}
