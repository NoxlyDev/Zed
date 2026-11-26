plugins {
    id("com.android.application")
}

android {
    namespace = "dev.zedandroid.editor"
    compileSdk = 34

    defaultConfig {
        applicationId = "dev.zedandroid.editor"
        minSdk = 26
        targetSdk = 34
        versionCode = 1
        versionName = "0.1.0"
        ndk { abiFilters += listOf("arm64-v8a") }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            // Debug-signed so the APK installs directly. Replace for Play Store.
            signingConfig = signingConfigs.getByName("debug")
        }
    }

    sourceSets {
        getByName("main") {
            // Filled by `cargo ndk -o android/app/src/main/jniLibs`.
            jniLibs.srcDir("src/main/jniLibs")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    packaging {
        jniLibs { useLegacyPackaging = true }
    }
}

dependencies {
    implementation("androidx.appcompat:appcompat:1.7.0")
    implementation("androidx.core:core:1.13.1")
    // Do NOT enable prefab: android-activity ships its own native glue.
    implementation("androidx.games:games-activity:4.4.0")
}
