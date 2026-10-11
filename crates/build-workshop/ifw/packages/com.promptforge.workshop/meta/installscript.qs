// Adds Workshop's launcher entry on Linux. The file name is Tauri's
// identifier, which GNOME on Wayland matches a window's application id
// against.

function Component() {}

// A quoted desktop-entry Exec argument: the spec's reserved characters
// backslash-escaped and `%` doubled, since it starts a field code anywhere
// on the line.
function desktopExec(path) {
    return "\"" + path.replace(/["`$\\]/g, "\\$&").replace(/%/g, "%%") + "\"";
}

Component.prototype.createOperations = function () {
    component.createOperations();

    if (systemInfo.kernelType === "linux") {
        var target = installer.value("TargetDir");
        component.addOperation("CreateDesktopEntry", "com.promptforge.workshop.desktop", [
            "Type=Application",
            "Name=PromptForge",
            "Comment=Write and run prompts",
            "Exec=" + desktopExec(target + "/{{WORKSHOP_EXECUTABLE}}"),
            "Icon=" + target + "/promptforge-workshop.png",
            "Terminal=false",
            "Categories=Development;"
        ].join("\n"));
    }
};
