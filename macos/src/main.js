{
	const scalingModeSelect = document.querySelector("select[name='scaling-mode']");
	const videoFolderButton = document.querySelector("button[name='video-folder']");
	const grid = document.querySelector(".grid");
	let appliedScalingMode = scalingModeSelect.value;
	let appliedVideo = "";

	videoFolderButton.addEventListener("click", () => {
		window.ipc.postMessage("video-folder:choose");
	});

	grid.addEventListener("click", (event) => {
		const videoButton = event.target.closest("button[name='video']");
		if (!videoButton || !grid.contains(videoButton)) {
			return;
		}
		if (videoButton.value === appliedVideo && scalingModeSelect.value === appliedScalingMode) {
			return;
		}
		appliedScalingMode = scalingModeSelect.value;
		appliedVideo = videoButton.value;
		for (const nextVideoButton of grid.querySelectorAll("button[name='video']")) {
			nextVideoButton.setAttribute(
				"aria-pressed",
				String(nextVideoButton.value === appliedVideo)
			);
		}
		window.ipc.postMessage("select:" + appliedVideo);
	});

	scalingModeSelect.addEventListener("change", () => {
		appliedScalingMode = scalingModeSelect.value;
		window.ipc.postMessage("scaling-mode:" + appliedScalingMode);
	});

	window.updateVideos = (videoFolder, tiles) => {
		appliedVideo = "";
		videoFolderButton.textContent = videoFolder;
		videoFolderButton.title = videoFolder;
		grid.innerHTML = tiles;
	};
}
