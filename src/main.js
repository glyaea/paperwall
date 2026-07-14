const grid = document.querySelector(".grid");
const scalingModeSelect = document.querySelector("select[name=\"scaling-mode\"]");
const videoFolderButton = document.querySelector("button[name=\"video-folder\"]");
let selectedVideo = "";

function postMessage(message) {
	window.ipc.postMessage(JSON.stringify(message));
}

videoFolderButton.addEventListener("click", () => {
	postMessage({ type: "pick_video_folder" });
});

scalingModeSelect.addEventListener("change", () => {
	postMessage({
		scaling_mode: scalingModeSelect.value,
		type: "update_scaling_mode"
	});
});

grid.addEventListener("click", (event) => {
	const tile = event.target.closest(".tile");
	if (!tile || tile.dataset.videoIndex === selectedVideo) {
		return;
	}
	selectedVideo = tile.dataset.videoIndex;
	for (const existingTile of grid.querySelectorAll(".tile")) {
		existingTile.setAttribute(
			"aria-pressed",
			String(existingTile.dataset.videoIndex === selectedVideo)
		);
	}
	postMessage({
		index: Number(selectedVideo),
		type: "select_video"
	});
});

window.paperwall = {
	setVideos(videoFolder, tiles) {
		selectedVideo = "";
		videoFolderButton.textContent = videoFolder;
		videoFolderButton.title = videoFolder;
		grid.innerHTML = tiles;
		window.paperwallPlatform.updateThumbnails(grid);
	}
};

window.paperwallPlatform.updateThumbnails(document);
