const { withAppBuildGradle, withGradleProperties } = require("expo/config-plugins");

const gate = `tasks.matching { it.name == "preReleaseBuild" }.configureEach {
    dependsOn(":clipper-clipboard:lintRelease")
}`;

module.exports = function clipboardLint(config) {
  config = withAppBuildGradle(config, (mod) => {
    if (!mod.modResults.contents.includes(gate)) mod.modResults.contents += `\n${gate}\n`;
    return mod;
  });
  return withGradleProperties(config, (mod) => {
    const key = "android.experimental.lint.analysisPerComponent";
    mod.modResults = mod.modResults.filter(
      (entry) => entry.type !== "property" || entry.key !== key,
    );
    mod.modResults.push({ type: "property", key, value: "false" });
    return mod;
  });
};
