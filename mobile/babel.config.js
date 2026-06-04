/**
 * @param {{ cache: (enabled: boolean) => void }} api
 */
module.exports = function (api) {
  api.cache(true);
  return {
    plugins: [
      [
        "@tamagui/babel-plugin",
        {
          components: ["tamagui"],
          config: "./src/tamagui.config.ts",
        },
      ],
    ],
    presets: ["babel-preset-expo"],
  };
};
