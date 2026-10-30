plugins {
    id("com.android.library")
}

android {
    namespace = "io.github.saatvik333.ghostcaret.ime"
    compileSdk = 36

    defaultConfig {
        minSdk = 26
        consumerProguardFiles("consumer-rules.pro")
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

dependencies {
    implementation("androidx.core:core:1.18.0")
    implementation("androidx.customview:customview:1.2.0")
    testImplementation("junit:junit:4.13.2")
}
