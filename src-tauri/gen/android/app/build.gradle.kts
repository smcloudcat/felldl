import java.util.Properties
import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("rust")
}

val tauriProperties = Properties().apply {
    val propFile = file("tauri.properties")
    if (propFile.exists()) {
        propFile.inputStream().use { load(it) }
    }
}

// 签名材料不入库：优先读取 gen/android/keystore.properties（已 gitignore），其次读环境变量。
// 不要把 .jks 或口令提交到 Git；示例见 keystore.properties.example。
val signingProperties = Properties().apply {
    val propFile = rootProject.file("keystore.properties")
    if (propFile.exists()) {
        propFile.inputStream().use { load(it) }
    }
}

fun signingValue(property: String, envName: String): String? =
    (signingProperties.getProperty(property) ?: System.getenv(envName))
        ?.trim()
        ?.takeIf { it.isNotEmpty() }

val releaseStoreFile = signingValue("storeFile", "FELLDL_KEYSTORE_FILE")
    ?.let { rootProject.file(it) }
    ?.takeIf { it.exists() }
val releaseStorePassword = signingValue("storePassword", "FELLDL_KEYSTORE_PASSWORD")
val releaseKeyAlias = signingValue("keyAlias", "FELLDL_KEY_ALIAS")
val releaseKeyPassword = signingValue("keyPassword", "FELLDL_KEY_PASSWORD")
val hasReleaseSigning = releaseStoreFile != null &&
    releaseStorePassword != null &&
    releaseKeyAlias != null &&
    releaseKeyPassword != null

android {
    compileSdk = 36
    namespace = "com.felldl.app"
    signingConfigs {
        if (hasReleaseSigning) {
            create("release") {
                storeFile = releaseStoreFile
                storePassword = releaseStorePassword
                keyAlias = releaseKeyAlias
                keyPassword = releaseKeyPassword
            }
        }
    }
    defaultConfig {
        manifestPlaceholders["usesCleartextTraffic"] = "false"
        applicationId = "com.felldl.app"
        minSdk = 24
        targetSdk = 36
        // 不提供默认值：以前缺文件会静默变成 1.0 / 1，升级时 versionCode 不涨，
        // Android 直接拒装且很难查。宁可在这里构建失败，也不要产出装不上的包。
        versionCode = (tauriProperties.getProperty("tauri.android.versionCode")
            ?: error("缺少 app/tauri.properties 的 tauri.android.versionCode；请用 scripts/build-android.ps1 构建，或先执行 cargo tauri android build"))
            .toInt()
        versionName = tauriProperties.getProperty("tauri.android.versionName")
            ?: error("缺少 app/tauri.properties 的 tauri.android.versionName；请用 scripts/build-android.ps1 构建")
    }
    buildTypes {
        getByName("debug") {
            manifestPlaceholders["usesCleartextTraffic"] = "true"
            isDebuggable = true
            isJniDebuggable = true
            isMinifyEnabled = false
            packaging {
                jniLibs.keepDebugSymbols.add("*/arm64-v8a/*.so")
                jniLibs.keepDebugSymbols.add("*/armeabi-v7a/*.so")
                jniLibs.keepDebugSymbols.add("*/x86/*.so")
                jniLibs.keepDebugSymbols.add("*/x86_64/*.so")
            }
        }
        getByName("release") {
            if (hasReleaseSigning) {
                signingConfig = signingConfigs.getByName("release")
            } else {
                logger.warn(
                    "[Felldl] 未找到 release 签名配置：keystore.properties 缺失或字段不全，release 包将为未签名状态。" +
                        " 参考 keystore.properties.example，不要把密钥或口令提交到 Git。"
                )
            }
            optimization {
               enable = true
            }
            proguardFiles(
                *fileTree(".") {
                  include("**/*.pro")
                  exclude("build/**")
                }.files.toTypedArray()
            )
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }
    buildFeatures {
        buildConfig = true
    }
}

kotlin {
    compilerOptions {
        jvmTarget = JvmTarget.JVM_1_8
    }
}

rust {
    rootDirRel = "../../../"
}

dependencies {
    implementation("androidx.webkit:webkit:1.14.0")
    implementation("androidx.appcompat:appcompat:1.7.1")
    implementation("androidx.activity:activity-ktx:1.10.1")
    implementation("com.google.android.material:material:1.12.0")
    implementation("androidx.lifecycle:lifecycle-process:2.10.0")
    testImplementation("junit:junit:4.13.2")
    androidTestImplementation("androidx.test.ext:junit:1.1.4")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.5.0")
}

apply(from = file("tauri.build.gradle.kts"))
