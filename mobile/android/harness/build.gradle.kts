plugins {
    id("com.android.application")
}

/** Builds the engine library with cargo-ndk into the directory AGP packages. */
abstract class CargoNdk : Exec() {
    @get:OutputDirectory
    abstract val outputDir: DirectoryProperty

    init {
        executable = "cargo"
        argumentProviders.add(
            CommandLineArgumentProvider {
                listOf(
                    "ndk", "--target", "arm64-v8a", "--platform", "26",
                    "--output-dir", outputDir.get().asFile.path,
                    "build", "--release",
                )
            },
        )
        // Cargo decides what to rebuild.
        outputs.upToDateWhen { false }
    }
}

android {
    namespace = "io.github.saatvik333.ghostcaret.harness"
    compileSdk = 36
    ndkVersion = "28.2.13676358"

    defaultConfig {
        applicationId = "io.github.saatvik333.ghostcaret.harness"
        minSdk = 26
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"
        ndk { abiFilters += "arm64-v8a" }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"))
            // A development harness: signed with the debug key so it installs over adb.
            signingConfig = signingConfigs.getByName("debug")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

val repositoryRoot = rootDir.resolve("../..").canonicalFile

val cargoNdk = tasks.register<CargoNdk>("cargoNdk") {
    description = "Builds libopen_flowtype_lib.so for arm64-v8a."
    workingDir = rootDir.resolve("rust")
    environment("CARGO_TARGET_DIR", repositoryRoot.resolve("target/android").path)
    environment("ANDROID_NDK_HOME", androidComponents.sdkComponents.ndkDirectory.get().asFile.path)
}

androidComponents {
    onVariants { variant ->
        variant.sources.jniLibs?.addGeneratedSourceDirectory(cargoNdk, CargoNdk::outputDir)
    }
}

dependencies {
    implementation(project(":ime"))
}
