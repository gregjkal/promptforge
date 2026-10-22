// Runs the installed Gateway's `init` once its payload is extracted, so the
// default configuration exists and, with Speech to Text selected, the speech
// runtime and models are downloaded into the installing user's profile.
// DisableAuthorizationFallback keeps the installer, and so `init`, from
// running as root.

function Component() {}

// A quoted desktop-entry Exec argument, in the spec's two layers: the
// reserved characters backslash-escaped inside the quotes, then every
// backslash doubled for the string escape, which readers undo first and
// which rejects `\$` and `\"`. IFW writes the line verbatim. `%` is doubled,
// since it starts a field code anywhere on the line.
function desktopExec(path) {
    var quoted = "\"" + path.replace(/["`$\\]/g, "\\$&") + "\"";
    return quoted.replace(/\\/g, "\\\\").replace(/%/g, "%%");
}

Component.prototype.createOperations = function () {
    component.createOperations();

    var target = installer.value("TargetDir");
    var gateway = target + "/{{GATEWAY_EXECUTABLE}}";
    var speech = installer.componentByName("com.runvil.stt").installationRequested();
    var command = speech ? "init" : "init --no-stt";
    // Execute reads `{0}` as the allowed exit codes, removes the
    // `workingdirectory=` and `errormessage=` arguments, and reads a last
    // `showStandardError` as merging stderr into the output Show Details
    // streams, so `init`'s progress lines and its error chain both show. The
    // last one is undocumented: https://github.com/qtproject/installer-framework/blob/4.11.0/src/libs/installer/elevatedexecuteoperation.cpp#L93-L113
    // The error box never carries that output, and it is modal over Show
    // Details, so its text sends the user to a terminal for the reason.
    var execute = ["{0}", gateway, "init"];
    if (!speech) {
        execute.push("--no-stt");
    }
    execute.push("workingdirectory=" + target);
    execute.push("errormessage=Runvil Gateway is installed, but its first-run setup "
        + "failed. To see why, choose Ignore, then run this in a terminal: \"" + gateway
        + "\" " + command + ". Fix what it reports and run it again. Retry runs the setup "
        + "again now. Cancel removes the whole installation.");
    execute.push("showStandardError");
    component.addOperation("Execute", execute);

    if (systemInfo.kernelType === "linux") {
        component.addOperation("CreateDesktopEntry", "com.runvil.gateway.desktop", [
            "Type=Application",
            "Name=Runvil Gateway Settings",
            "Comment=Open the Runvil Gateway Settings page",
            "Exec=" + desktopExec(gateway) + " --browser",
            "Icon=" + target + "/runvil-gateway.png",
            "Terminal=false",
            "Categories=Development;"
        ].join("\n"));
    }
};
