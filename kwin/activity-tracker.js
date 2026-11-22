const service = "dev.requiem.Activity";
const path = "/dev/requiem/Activity";
let current = null;

function report() {
  const w = current;
  callDBus(service, path, service, "WindowActivated",
    w ? String(w.caption) : "", w ? String(w.resourceClass) : "", w ? Boolean(w.fullScreen) : false);
}

function track(w) {
  if (current) {
    current.captionChanged.disconnect(report);
    current.fullScreenChanged.disconnect(report);
  }
  current = w;
  if (w) {
    w.captionChanged.connect(report);
    w.fullScreenChanged.connect(report);
  }
  report();
}

workspace.windowActivated.connect(track);
track(workspace.activeWindow);
