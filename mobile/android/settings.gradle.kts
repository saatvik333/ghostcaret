pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "ghostcaret-android"

// The keyboard, packaged as a library so the Tauri Android project can
// include it unchanged.
include(":ime")
// A standalone APK around the keyboard for development and device tests.
include(":harness")
