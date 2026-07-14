#[cfg(not(any(target_os = "macos", target_os = "windows")))]
compile_error!("PaperWall supports macOS and Windows.");

#[cfg(target_os = "macos")]
#[path = "macos/app.rs"]
mod platform;

#[cfg(target_os = "windows")]
#[path = "windows/app.rs"]
mod platform;

use rfd::FileDialog;
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopWindowTarget};
use tao::window::{Window, WindowBuilder, WindowId};

const MAIN_CSS: &str = include_str!("main.css");
const MAIN_HTML: &str = include_str!("main.html");
const MAIN_JS: &str = include_str!("main.js");
const PICKER_HEIGHT: f64 = 600.0;
const PICKER_WIDTH: f64 = 800.0;

struct App {
	picker: platform::Picker,
	selected_video: Option<usize>,
	settings: Settings,
	settings_path: PathBuf,
	videos: Arc<Mutex<Vec<Video>>>,
	wallpaper: Option<platform::Wallpaper>
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

#[derive(Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
enum UserEvent {
	PickVideoFolder,
	SelectVideo { index: usize },
	UpdateScalingMode { scaling_mode: ScalingMode }
}

#[derive(Clone)]
struct Video {
	name: String,
	path: PathBuf
}

fn create_picker_html(video_folder: &Path, scaling_mode: ScalingMode, tiles: &str) -> String {
	MAIN_HTML
		.replace("{{video_folder}}", &escape_html(&video_folder.display().to_string()))
		.replace("{{scaling_mode_options}}", &create_scaling_mode_options(scaling_mode))
		.replace("{{tiles}}", tiles)
}

fn create_picker_window(
	event_loop: &EventLoopWindowTarget<UserEvent>
) -> Result<Window, tao::error::OsError> {
	let size = LogicalSize::new(PICKER_WIDTH, PICKER_HEIGHT);
	WindowBuilder::new()
		.with_inner_size(size)
		.with_min_inner_size(size)
		.with_title("PaperWall")
		.build(event_loop)
}

fn create_scaling_mode_options(scaling_mode: ScalingMode) -> String {
	let mut options = String::new();
	for option in [ScalingMode::FillScreen, ScalingMode::FitToScreen] {
		if option == scaling_mode {
			let _ = write!(options, "<option selected>{}</option>", option.label());
			continue;
		}
		let _ = write!(options, "<option>{}</option>", option.label());
	}
	options
}

fn create_tiles(videos: &[Video]) -> Result<String, Box<dyn Error>> {
	let mut tiles = String::new();
	for (index, video) in videos.iter().enumerate() {
		let name = escape_html(&video.name);
		let thumbnail = platform::create_thumbnail(video, index)?;
		let _ = write!(
			tiles,
			concat!(
				"<button aria-label=\"{}\" aria-pressed=\"false\" class=\"tile\" ",
				"data-video-index=\"{}\" title=\"{}\" type=\"button\">{}</button>"
			),
			name,
			index,
			name,
			thumbnail
		);
	}
	Ok(tiles)
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
	path.extension()
		.and_then(|extension| extension.to_str())
		.is_some_and(|extension| extension.eq_ignore_ascii_case("mp4"))
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
		let path = entry?.path();
		if path.is_file() && is_video_path(&path) {
			let name = path.file_name().unwrap().to_string_lossy().to_string();
			videos.push(Video { name, path });
		}
	}
	videos.sort_by(|left, right| left.name.cmp(&right.name));
	Ok(videos)
}

fn update_scaling_mode(app: &mut App, scaling_mode: ScalingMode) -> Result<(), Box<dyn Error>> {
	if scaling_mode == app.settings.scaling_mode {
		return Ok(());
	}
	let settings = Settings {
		scaling_mode,
		video_folder: app.settings.video_folder.clone()
	};
	write_settings(&app.settings_path, &settings)?;
	app.settings = settings;
	if let Some(wallpaper) = &mut app.wallpaper {
		wallpaper.set_scaling_mode(scaling_mode)?;
	}
	Ok(())
}

fn update_video_folder(app: &mut App, video_folder: PathBuf) -> Result<(), Box<dyn Error>> {
	let videos = read_videos(&video_folder)?;
	let tiles = create_tiles(&videos)?;
	let html = create_picker_html(&video_folder, app.settings.scaling_mode, &tiles);
	let settings = Settings {
		scaling_mode: app.settings.scaling_mode,
		video_folder
	};
	write_settings(&app.settings_path, &settings)?;
	app.settings = settings;
	*app.videos.lock().unwrap() = videos;
	app.selected_video = None;
	app.picker.set_videos(html, &app.settings.video_folder, &tiles)?;
	Ok(())
}

fn update_wallpaper(
	app: &mut App,
	event_loop: &EventLoopWindowTarget<UserEvent>,
	index: usize
) -> Result<(), Box<dyn Error>> {
	if app.selected_video == Some(index) {
		return Ok(());
	}
	let path = app.videos.lock().unwrap().get(index).map(|video| video.path.clone());
	let Some(path) = path else {
		return Ok(());
	};
	if let Some(wallpaper) = &mut app.wallpaper {
		wallpaper.set_video(&path, app.settings.scaling_mode)?;
	} else {
		app.wallpaper = Some(platform::Wallpaper::create(
			event_loop,
			&path,
			app.settings.scaling_mode
		)?);
	}
	app.selected_video = Some(index);
	Ok(())
}

fn write_settings(path: &Path, settings: &Settings) -> Result<(), Box<dyn Error>> {
	fs::create_dir_all(path.parent().ok_or("Settings path has no parent")?)?;
	fs::write(path, format!("{}\n", serde_json::to_string_pretty(settings)?))?;
	Ok(())
}

fn run() -> Result<(), Box<dyn Error>> {
	let settings_path = platform::read_settings_path()?;
	let (settings, should_write_settings) = if let Some(settings) = read_settings(&settings_path)? {
		(settings, false)
	} else {
		(
			Settings {
				scaling_mode: ScalingMode::FillScreen,
				video_folder: platform::read_default_video_folder()?
			},
			true
		)
	};
	let videos = read_videos(&settings.video_folder)?;
	let tiles = create_tiles(&videos)?;
	let html = create_picker_html(&settings.video_folder, settings.scaling_mode, &tiles);
	let videos = Arc::new(Mutex::new(videos));
	let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
	let proxy = event_loop.create_proxy();
	let window = create_picker_window(&event_loop)?;
	let picker = platform::Picker::create(window, proxy, html, Arc::clone(&videos))?;
	if should_write_settings {
		write_settings(&settings_path, &settings)?;
	}
	let mut app = App {
		picker,
		selected_video: None,
		settings,
		settings_path,
		videos,
		wallpaper: None
	};
	event_loop.run(move |event, event_loop, control_flow| {
		*control_flow = ControlFlow::Wait;
		match event {
			Event::WindowEvent {
				event: WindowEvent::CloseRequested,
				window_id,
				..
			} if window_id == app.picker.window_id() => *control_flow = ControlFlow::Exit,
			Event::UserEvent(UserEvent::PickVideoFolder) => {
				if let Some(video_folder) = FileDialog::new()
					.set_directory(&app.settings.video_folder)
					.pick_folder()
				{
					if let Err(error) = update_video_folder(&mut app, video_folder) {
						eprintln!("Updating video folder failed | {error}");
					}
				}
			}
			Event::UserEvent(UserEvent::SelectVideo { index }) => {
				if let Err(error) = update_wallpaper(&mut app, event_loop, index) {
					eprintln!("Updating wallpaper failed | {error}");
				}
			}
			Event::UserEvent(UserEvent::UpdateScalingMode { scaling_mode }) => {
				if let Err(error) = update_scaling_mode(&mut app, scaling_mode) {
					eprintln!("Updating scaling mode failed | {error}");
				}
			}
			_ => {}
		}
	});
}

impl ScalingMode {
	fn label(self) -> &'static str {
		match self {
			Self::FillScreen => "Fill Screen",
			Self::FitToScreen => "Fit to Screen"
		}
	}
}

fn main() -> Result<(), Box<dyn Error>> {
	run()
}
