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
    var speech = installer.componentByName("com.promptforge.stt").installationRequested();
    var command = speech ? "init" : "init --no-stt";
    // Execute reads `{0}` as the allowed exit codes, removes the
    // `workingdirectory=` and `errormessage=` arguments, and reads a last
    // `showStandardError` as merging stderr into the output Show Details
    // streams, so `init`'s progress lines and its error chain both show. The
    // last one is undocumented: https://github.com/qtproject/installer-framework/blob/4.11.0/src/libs/installer/elevatedexecuteoperation.cpp#L93-L113
    var execute = ["{0}", gateway, "init"];
    if (!speech) {
        execute.push("--no-stt");
    }
    execute.push("workingdirectory=" + target);
    execute.push("errormessage=PromptForge Gateway is installed, but its first-run setup, \""
        + gateway + "\" " + command + ", failed; Show Details has its output. Retry runs it "
        + "again. Ignore finishes the installation with the Gateway installed: its next start "
        + "sets up what is missing, or run the same command again later. Cancel removes the "
        + "whole installation.");
    execute.push("showStandardError");
    component.addOperation("Execute", execute);

    if (systemInfo.kernelType === "linux") {
        component.addOperation("CreateDesktopEntry", "com.promptforge.gateway.desktop", [
            "Type=Application",
            "Name=PromptForge Gateway Settings",
            "Comment=Open the PromptForge Gateway Settings page",
            "Exec=" + desktopExec(gateway) + " --browser",
            "Icon=" + target + "/promptforge-gateway.png",
            "Terminal=false",
            "Categories=Development;"
        ].join("\n"));
    }
};
