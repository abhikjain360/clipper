module.exports = {
  dependency: {
    platforms: {
      android: {
        cmakeListsPath: "build/generated/source/codegen/jni/CMakeLists.txt",
        sourceDir: "./android",
      },
      ios: null,
    },
  },
};
