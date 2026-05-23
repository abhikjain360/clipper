pluginManagement {
    val flutterSdkPath =
        run {
            val properties = java.util.Properties()
            file("local.properties").inputStream().use { properties.load(it) }
            val flutterSdkPath = properties.getProperty("flutter.sdk")
            require(flutterSdkPath != null) { "flutter.sdk not set in local.properties" }
            flutterSdkPath
        }

    val flutterGradleBuild = file("$flutterSdkPath/packages/flutter_tools/gradle")
    val writableFlutterGradleBuild =
        if (flutterGradleBuild.canWrite()) {
            flutterGradleBuild
        } else {
            val cachedFlutterSdk = file(".gradle/flutter_sdk")
            val cachedFlutterGradleBuild =
                file(".gradle/flutter_sdk/packages/flutter_tools/gradle")
            val sourceMarker = file(".gradle/flutter_sdk.source")
            val sourcePath = flutterGradleBuild.absolutePath

            if (
                !cachedFlutterGradleBuild.isDirectory ||
                    !sourceMarker.isFile ||
                    sourceMarker.readText() != sourcePath
            ) {
                cachedFlutterSdk.deleteRecursively()
                flutterGradleBuild.copyRecursively(cachedFlutterGradleBuild)
                val engineVersion = file("$flutterSdkPath/bin/internal/engine.version")
                val cachedEngineVersion = file(".gradle/flutter_sdk/bin/internal/engine.version")
                cachedEngineVersion.parentFile.mkdirs()
                engineVersion.copyTo(cachedEngineVersion, overwrite = true)
                sourceMarker.parentFile.mkdirs()
                sourceMarker.writeText(sourcePath)
            }

            cachedFlutterGradleBuild
        }

    includeBuild(writableFlutterGradleBuild.absolutePath)

    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

plugins {
    id("dev.flutter.flutter-plugin-loader") version "1.0.0"
    id("com.android.application") version "9.0.1" apply false
    id("org.jetbrains.kotlin.android") version "2.3.20" apply false
}

fun remapReadOnlyProjectDir(project: org.gradle.api.initialization.ProjectDescriptor) {
    val projectDir = project.projectDir
    if (!projectDir.isDirectory || projectDir.canWrite()) {
        return
    }

    val safeName = project.path.removePrefix(":").replace(':', '_')
    val cachedProjectDir = file(".gradle/flutter_plugin_projects/$safeName")
    val sourceMarker = file(".gradle/flutter_plugin_projects/$safeName.source")
    val sourcePath = projectDir.absolutePath

    if (
        !cachedProjectDir.isDirectory ||
            !sourceMarker.isFile ||
            sourceMarker.readText() != sourcePath
    ) {
        cachedProjectDir.deleteRecursively()
        projectDir.copyRecursively(cachedProjectDir)
        sourceMarker.parentFile.mkdirs()
        sourceMarker.writeText(sourcePath)
    }

    project.projectDir = cachedProjectDir
}

rootProject.children.forEach(::remapReadOnlyProjectDir)

include(":app")
