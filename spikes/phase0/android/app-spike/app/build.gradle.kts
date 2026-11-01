plugins {
    id("com.android.application")
}

android {
    namespace = "lk.networkconnector.spike"
    compileSdk = 36

    defaultConfig {
        applicationId = "lk.networkconnector.spike"
        minSdk = 26
        targetSdk = 36
        versionCode = 1
        versionName = "0.0.1-spike"
        ndk { abiFilters += listOf("arm64-v8a") }
    }

    // One flavor per Wasm runtime. ../build-so.sh puts each .so in src/<flavor>/jniLibs/arm64-v8a/.
    flavorDimensions += "runtime"
    productFlavors {
        create("pulley") { dimension = "runtime"; applicationIdSuffix = ".pulley" }
        create("cranelift") { dimension = "runtime"; applicationIdSuffix = ".cranelift" }
        create("wasmi") { dimension = "runtime"; applicationIdSuffix = ".wasmi" }
    }

    buildTypes {
        // Spike only: release is signed with the debug key so the owner can sideload it.
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.getByName("debug")
        }
    }

    packaging {
        // The .so files are already stripped by cargo (profile.release strip = true).
        jniLibs { keepDebugSymbols += "**/libnc_wasm_spike.so" }
    }
}
