{
	const scalingSelect = document.querySelector("select[name='scaling']");
	const videoButtons = document.querySelectorAll("button[name='video']");
	let appliedScaling = scalingSelect.value;
	let appliedVideo = "";

	for (const videoButton of videoButtons) {
		videoButton.addEventListener("click", () => {
			if (videoButton.value === appliedVideo && scalingSelect.value === appliedScaling) {
				return;
			}
			appliedScaling = scalingSelect.value;
			appliedVideo = videoButton.value;
			for (const nextVideoButton of videoButtons) {
				nextVideoButton.setAttribute(
					"aria-pressed",
					String(nextVideoButton.value === appliedVideo)
				);
			}
			window.ipc.postMessage("select:" + appliedVideo);
		});
	}

	scalingSelect.addEventListener("change", () => {
		appliedScaling = scalingSelect.value;
		window.ipc.postMessage("scaling:" + appliedScaling);
	});
}
