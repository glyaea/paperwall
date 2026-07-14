const grid = document.querySelector(".grid");
const scalingModeSelect = document.querySelector("select[name=\"scaling-mode\"]");
const videoFolderButton = document.querySelector("button[name=\"video-folder\"]");

function postMessage(message) {
	window.ipc.postMessage(JSON.stringify(message));
}

function updateThumbnails(root = document) {
	for (const video of root.querySelectorAll(".tile video")) {
		video.addEventListener("loadedmetadata", () => {
			if (Number.isFinite(video.duration)) {
				video.currentTime = Math.floor(video.duration / 2);
			}
		}, { once: true });
		video.addEventListener("seeked", () => video.pause(), { once: true });
	}
}

videoFolderButton.addEventListener("click", () => {
	postMessage({ type: "pick_video_folder" });
});

scalingModeSelect.addEventListener("change", () => {
	postMessage({
		type: "update_scaling_mode",
		scaling_mode: scalingModeSelect.value
	});
});

grid.addEventListener("click", (event) => {
	const tile = event.target.closest(".tile");
	if (!tile) {
		return;
	}
	for (const existingTile of grid.querySelectorAll(".tile")) {
		existingTile.setAttribute("aria-pressed", "false");
	}
	tile.setAttribute("aria-pressed", "true");
	postMessage({
		type: "select_video",
		path: tile.dataset.videoPath
	});
});

window.paperwall = {
	setVideoFolder(videoFolder, tiles) {
		videoFolderButton.textContent = videoFolder;
		videoFolderButton.title = videoFolder;
		grid.innerHTML = tiles;
		updateThumbnails(grid);
	}
};

updateThumbnails();
