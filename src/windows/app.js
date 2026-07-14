window.paperwallPlatform = {
	updateThumbnails(root) {
		for (const video of root.querySelectorAll(".tile video")) {
			video.addEventListener("loadedmetadata", () => {
				if (Number.isFinite(video.duration)) {
					video.currentTime = Math.floor(video.duration / 2);
				}
			}, { once: true });
			video.addEventListener("seeked", () => video.pause(), { once: true });
		}
	}
};
